use super::discovery::{
    V2ModelList, catalog_from_v2_models, command_names, commands_from_wire, models_from_providers,
    skills_from_wire,
};
use super::feed::{
    PendingSpawn, SessionFeed, bind_child, map_questions, part_delta_events, part_snapshot_events,
    task_completion, tool_call,
};
use super::server::{Server, encode_directory, http_client};
use super::v2::{
    MAX_PENDING_V2_TOOLS, MAX_V2_SESSION_MODELS, normalize_v2_frame,
    normalize_v2_frame_with_session_models,
};
use super::*;
use crate::{TodoItem, TodoStatus, ToolCall};

#[derive(Clone, Copy)]
enum NativeCommandReply {
    Http404,
    Disconnect,
    DelayedHttp404,
}

struct Fixture {
    queued: bool,
    v2: bool,
    permission: &'static str,
    answer: Option<bool>,
    version: &'static str,
    overrides: Value,
    command_failure: bool,
    native_command_reply: Option<NativeCommandReply>,
}

impl Default for Fixture {
    fn default() -> Self {
        Self {
            queued: false,
            v2: false,
            permission: "full_access",
            answer: None,
            version: "2.0.3",
            overrides: json!({}),
            command_failure: false,
            native_command_reply: None,
        }
    }
}

struct TurnWire {
    bus: mpsc::UnboundedSender<Value>,
    posts: Arc<std::sync::Mutex<Vec<(String, Value)>>>,
    requests: mpsc::UnboundedReceiver<String>,
    events: mpsc::Receiver<Result<AgentEvent, HarnessError>>,
    interrupt: tokio_util::sync::CancellationToken,
    steering: Option<mpsc::Sender<crate::SteerMessage>>,
    polls: Arc<std::sync::atomic::AtomicUsize>,
    command_failure_release: Option<tokio::sync::oneshot::Sender<()>>,
    server: tokio::task::JoinHandle<()>,
    run: tokio::task::JoinHandle<()>,
}

impl Drop for TurnWire {
    fn drop(&mut self) {
        self.interrupt.cancel();
        self.run.abort();
        self.server.abort();
    }
}

fn steer(prompt: &str) -> crate::SteerMessage {
    crate::SteerMessage {
        prompt: prompt.into(),
        message_id: None,
        attachments: Vec::new(),
        skills: Vec::new(),
    }
}

impl TurnWire {
    async fn start(queued: bool) -> Self {
        Self::start_fixture(Fixture {
            queued,
            ..Fixture::default()
        })
        .await
    }

    async fn start_proto(queued: bool, v2: bool) -> Self {
        Self::start_fixture(Fixture {
            queued,
            v2,
            ..Fixture::default()
        })
        .await
    }

    async fn start_config(v2: bool, version: &'static str, overrides: Value) -> Self {
        Self::start_fixture(Fixture {
            v2,
            version,
            overrides,
            ..Fixture::default()
        })
        .await
    }

    async fn start_native_command(queued: bool, reply: NativeCommandReply) -> Self {
        Self::start_fixture(Fixture {
            queued,
            native_command_reply: Some(reply),
            ..Fixture::default()
        })
        .await
    }

    async fn start_fixture(fixture: Fixture) -> Self {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let Fixture {
            queued,
            v2,
            permission,
            answer,
            version,
            overrides,
            command_failure,
            native_command_reply,
        } = fixture;
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let (bus, bus_rx) = mpsc::unbounded_channel::<Value>();
        let bus_rx = Arc::new(tokio::sync::Mutex::new(Some(bus_rx)));
        let (request_tx, requests) = mpsc::unbounded_channel();
        let posts = Arc::new(std::sync::Mutex::new(Vec::new()));
        let recorded = posts.clone();
        let hold_prompt = overrides["holdPrompt"].as_bool().unwrap_or(false);
        let busy_polls = overrides["busyPolls"].as_u64().unwrap_or(0) as usize;
        let fail_prompt_delay = overrides["failPromptDelayMs"]
            .as_u64()
            .map(Duration::from_millis);
        let fail_permission_reply = overrides["failPermissionReply"].as_bool().unwrap_or(false);
        let no_bus = overrides["noBus"].as_bool().unwrap_or(false);
        let resume_failures = overrides["resumeFailures"].as_u64().unwrap_or(0) as usize;
        let resume_attempts = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let agents = overrides.get("agents").cloned();
        let polls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let server_polls = polls.clone();
        let (command_failure_release, command_failure_wait) = tokio::sync::oneshot::channel();
        let command_failure_wait = Arc::new(tokio::sync::Mutex::new(Some(command_failure_wait)));
        let server = tokio::spawn(async move {
            let mut connections = tokio::task::JoinSet::new();
            loop {
                let (mut socket, _) = listener.accept().await.unwrap();
                let bus_rx = bus_rx.clone();
                let request_tx = request_tx.clone();
                let recorded = recorded.clone();
                let polls = server_polls.clone();
                let command_failure_wait = command_failure_wait.clone();
                let resume_attempts = resume_attempts.clone();
                let agents = agents.clone();
                connections.spawn(async move {
                    let mut request = Vec::new();
                    let mut buffer = [0; 4096];
                    let header_end = loop {
                        let read = socket.read(&mut buffer).await.unwrap();
                        if read == 0 {
                            return;
                        }
                        request.extend_from_slice(&buffer[..read]);
                        if let Some(end) = request.windows(4).position(|w| w == b"\r\n\r\n") {
                            break end + 4;
                        }
                    };
                    let header = String::from_utf8_lossy(&request[..header_end]).to_string();
                    let is_post = header.starts_with("POST ") || header.starts_with("PATCH ");
                    let target = header.lines().next().unwrap().split_whitespace().nth(1).unwrap();
                    let path = target.split('?').next().unwrap().to_owned();
                    let length = header
                        .lines()
                        .find_map(|line| {
                            let (name, value) = line.split_once(':')?;
                            name.eq_ignore_ascii_case("content-length")
                                .then(|| value.trim().parse::<usize>().unwrap())
                        })
                        .unwrap_or(0);
                    while request.len() < header_end + length {
                        let read = socket.read(&mut buffer).await.unwrap();
                        if read == 0 {
                            return;
                        }
                        request.extend_from_slice(&buffer[..read]);
                    }
                    if is_post {
                        recorded.lock().unwrap().push((
                            path.clone(),
                            serde_json::from_slice(&request[header_end..header_end + length])
                                .unwrap_or(Value::Null),
                        ));
                    }
                    let respond = |status: &str, body: &str| {
                        format!("HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len())
                    };
                    if no_bus && (path == "/global/event" || path == "/api/event") {
                        socket.write_all(respond("404 Not Found", "{}").as_bytes()).await.unwrap();
                        return;
                    }
                    if let Some(delay) = fail_prompt_delay
                        && (path.ends_with("/prompt_async") || path.ends_with("/prompt"))
                    {
                        request_tx.send(path).ok();
                        tokio::time::sleep(delay).await;
                        socket.write_all(respond("500 Internal Server Error", r#"{"error":"late"}"#).as_bytes()).await.ok();
                        return;
                    }
                    if fail_permission_reply && path.contains("/permission") {
                        socket.write_all(respond("500 Internal Server Error", r#"{"error":"gone"}"#).as_bytes()).await.unwrap();
                        return;
                    }
                    if path == "/session/flaky" {
                        let attempt = resume_attempts.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                        let (status, body) = if attempt < resume_failures {
                            ("500 Internal Server Error", r#"{"error":"migrating"}"#)
                        } else {
                            ("200 OK", r#"{"id":"flaky"}"#)
                        };
                        socket.write_all(respond(status, body).as_bytes()).await.unwrap();
                        return;
                    }
                    if path == "/agent" && let Some(agents) = &agents {
                        socket.write_all(respond("200 OK", &agents.to_string()).as_bytes()).await.unwrap();
                        return;
                    }
                    if path == "/global/event" || path == "/api/event" {
                        socket.write_all(b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nConnection: close\r\n\r\n: connected\n\n").await.unwrap();
                        let mut events = bus_rx.lock().await.take().unwrap();
                        while let Some(event) = events.recv().await {
                            if socket.write_all(format!("data: {event}\n\n").as_bytes()).await.is_err() {
                                break;
                            }
                        }
                        return;
                    }
                    if command_failure && is_post && path.ends_with("/command") {
                        socket.write_all(b"HTTP/1.1 400 Bad Request\r\nContent-Length: 12\r\nConnection: close\r\n\r\nbad command!").await.unwrap();
                        return;
                    }
                    if hold_prompt && (path.ends_with("/prompt_async") || path.ends_with("/prompt")) {
                        request_tx.send(path).ok();
                        std::future::pending::<()>().await;
                        return;
                    }
                    if path == "/session/status" || path == "/api/session/active" {
                        let count = polls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                        let body = if count < busy_polls {
                            r#"{"fixture":{"type":"busy"}}"#
                        } else {
                            r#"{"fixture":{"type":"idle"}}"#
                        };
                        socket.write_all(format!("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).as_bytes()).await.unwrap();
                        return;
                    }
                    let health = json!({"version": version}).to_string();
                    if let Some(reply) = native_command_reply
                        && is_post
                        && path == "/session/fixture/command"
                    {
                        request_tx.send(path.clone()).ok();
                        match reply {
                            NativeCommandReply::Disconnect => return,
                            NativeCommandReply::DelayedHttp404 => {
                                if let Some(wait) = command_failure_wait.lock().await.take() {
                                    wait.await.ok();
                                }
                            }
                            NativeCommandReply::Http404 => {}
                        }
                        let body = r#"{"error":"command removed"}"#;
                        socket.write_all(format!("HTTP/1.1 404 Not Found\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).as_bytes()).await.unwrap();
                        return;
                    }
                    let missing_resume = path == "/session/missing" || path == "/api/session/missing";
                    let (status, body) = if missing_resume {
                        ("404 Not Found", r#"{"error":"missing"}"#)
                    } else if v2 {
                        match path.as_str() {
                            "/api/health" => ("200 OK", health.as_str()),
                            "/api/session" => ("200 OK", r#"{"data":{"id":"fixture"}}"#),
                            "/api/command" => ("200 OK", r#"{"data":[]}"#),
                            "/api/model" => ("200 OK", r#"{"data":[{"providerID":"opencode","id":"muse","name":"Muse","limit":{"context":1000},"variants":[{"id":"low"}],"enabled":true},{"providerID":"opencode","id":"long-context","name":"Long Context","limit":{"context":2000},"enabled":true}]}"#),
                            _ => ("200 OK", "{}"),
                        }
                    } else {
                        match path.as_str() {
                            "/global/health" => ("200 OK", r#"{"healthy":true,"version":"1.18.31"}"#),
                            "/session" => ("200 OK", r#"{"id":"fixture"}"#),
                            "/command" => ("200 OK", "[]"),
                            _ => ("200 OK", "{}"),
                        }
                    };
                    socket.write_all(format!("HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).as_bytes()).await.unwrap();
                    if path.ends_with("/prompt_async")
                        || path.ends_with("/prompt")
                        || path.ends_with("/abort")
                        || path.ends_with("/interrupt")
                        || path == "/api/model"
                    {
                        request_tx.send(path).ok();
                    }
                });
            }
        });
        let (event_tx, events) = mpsc::channel(64);
        let (steer_tx, steering) = mpsc::channel(4);
        if queued {
            steer_tx.send(steer("second")).await.unwrap();
        }
        let retained_steering = overrides["keepSteering"]
            .as_bool()
            .unwrap_or(false)
            .then_some(steer_tx);
        let interrupt = tokio_util::sync::CancellationToken::new();
        let mut request = json!({
            "prompt": if native_command_reply.is_some() { "/project-review" } else { "first" },
            "cwd": "",
            "permission": permission,
            "model": if v2 { Some("opencode/muse") } else { None },
            "reasoning": "low",
        });
        request
            .as_object_mut()
            .unwrap()
            .extend(overrides.as_object().unwrap().clone());
        let request: RunRequest = serde_json::from_value(request).unwrap();
        let known_command = if native_command_reply.is_some() {
            "project-review"
        } else {
            "test"
        };
        let attached = Server::attached(base).unwrap();
        let run = tokio::spawn(run_session(Session {
            server: Box::pin(async move { Ok(attached) }),
            event_tx,
            controls: RunControls {
                request_input: Box::new(move |questions| {
                    let answer = answer.expect("fixture must not ask for input");
                    let (tx, rx) = tokio::sync::oneshot::channel();
                    tx.send(
                        questions
                            .into_iter()
                            .map(|question| {
                                let label = match (
                                    question.header == crate::claude::PERMISSION_HEADER,
                                    answer,
                                ) {
                                    (true, true) => crate::claude::PERMISSION_ALLOW,
                                    (true, false) => crate::claude::PERMISSION_DENY,
                                    (false, true) => "Yes",
                                    (false, false) => "No",
                                };
                                UserInputAnswer {
                                    question_id: question.id,
                                    labels: vec![label.into()],
                                }
                            })
                            .collect(),
                    )
                    .unwrap();
                    rx
                }),
                steering,
                interrupt: interrupt.clone(),
            },
            permission: request.permission.for_harness(HarnessId::Opencode),
            request,
            interrupt_grace: Duration::from_secs(2),
            kill_grace: Duration::from_millis(50),
            known_commands: Some(json!([{ "name": known_command }])),
            initial_native_command_selected: native_command_reply.is_some(),
        }));
        Self {
            bus,
            posts,
            requests,
            events,
            interrupt,
            steering: retained_steering,
            polls,
            command_failure_release: matches!(
                native_command_reply,
                Some(NativeCommandReply::DelayedHttp404)
            )
            .then_some(command_failure_release),
            server,
            run,
        }
    }

    async fn requests_any_order(&mut self, suffixes: &[&str]) {
        let mut seen = Vec::new();
        for _ in suffixes {
            seen.push(
                tokio::time::timeout(Duration::from_secs(5), self.requests.recv())
                    .await
                    .unwrap()
                    .unwrap(),
            );
        }
        for suffix in suffixes {
            assert!(
                seen.iter().any(|path| path.ends_with(suffix)),
                "missing {suffix}: {seen:?}"
            );
        }
    }

    async fn request(&mut self, suffix: &str) {
        let path = tokio::time::timeout(Duration::from_secs(5), self.requests.recv())
            .await
            .unwrap()
            .unwrap();
        assert!(path.ends_with(suffix), "unexpected request: {path}");
    }

    async fn posted(&self, suffix: &str) -> Value {
        tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                if let Some((_, body)) = self
                    .posts
                    .lock()
                    .unwrap()
                    .iter()
                    .find(|(path, _)| path.ends_with(suffix))
                {
                    return body.clone();
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("expected HTTP reply")
    }

    fn status(&self, status: &str) {
        self.bus.send(json!({"type":"session.status", "properties":{"sessionID":"fixture", "status":{"type":status}}})).unwrap();
    }

    fn idle(&self) {
        self.bus
            .send(json!({"type":"session.idle", "properties":{"sessionID":"fixture"}}))
            .unwrap();
    }

    fn v2(&self, kind: &str, data: Value) {
        self.bus
            .send(json!({"id": format!("evt_{kind}"), "type": kind, "data": data}))
            .unwrap();
    }

    async fn next_event(&mut self) -> AgentEvent {
        self.next_event_within(Duration::from_secs(5)).await
    }

    async fn next_event_within(&mut self, budget: Duration) -> AgentEvent {
        tokio::time::timeout(budget, self.events.recv())
            .await
            .unwrap()
            .unwrap()
            .unwrap()
    }

    async fn done(&mut self) -> (DoneStatus, String) {
        tokio::time::timeout(Duration::from_secs(5), async {
            let mut text = String::new();
            loop {
                match self.events.recv().await.unwrap().unwrap() {
                    AgentEvent::TextDelta { text: delta } => text.push_str(&delta),
                    AgentEvent::Done { status, .. } => return (status, text),
                    _ => {}
                }
            }
        })
        .await
        .unwrap()
    }
}

#[tokio::test]
async fn completed_turn_keeps_mailbox_alive_for_the_next_queued_request() {
    let mut wire = TurnWire::start_config(false, "1.18.21", json!({ "keepSteering": true })).await;
    wire.request("/prompt_async").await;
    wire.status("busy");
    wire.status("idle");
    wire.idle();
    assert_eq!(wire.done().await.0, DoneStatus::Completed);
    assert!(!wire.run.is_finished());
    wire.steering
        .as_ref()
        .unwrap()
        .send(steer("after completion"))
        .await
        .unwrap();
    wire.request("/prompt_async").await;
    wire.status("busy");
    wire.status("idle");
    wire.idle();
    assert_eq!(wire.done().await.0, DoneStatus::Completed);
    assert!(!wire.run.is_finished());
    drop(wire.steering.take());
    tokio::time::timeout(Duration::from_secs(5), &mut wire.run)
        .await
        .unwrap()
        .unwrap();
}

#[tokio::test]
async fn queued_turn_ignores_previous_turn_duplicate_idle() {
    for status_first in [true, false] {
        let mut wire = TurnWire::start(true).await;
        wire.requests_any_order(&["/prompt_async", "/abort"]).await;
        wire.status("busy");
        if status_first {
            wire.status("idle");
            wire.idle();
        } else {
            wire.idle();
            wire.status("idle");
        }
        wire.request("/prompt_async").await;
        wire.status("busy");
        wire.bus.send(json!({"type":"message.updated", "properties":{"info":{"id":"answer", "sessionID":"fixture", "role":"assistant"}}})).unwrap();
        wire.bus.send(json!({"type":"message.part.updated", "properties":{"part":{"id":"text", "messageID":"answer", "sessionID":"fixture", "type":"text", "text":"SECOND_OK"}}})).unwrap();
        wire.status("idle");
        let (status, text) = wire.done().await;
        assert_eq!(status, DoneStatus::Completed);
        assert_eq!(
            text, "SECOND_OK",
            "queued turn was completed before its response"
        );
    }
}

#[tokio::test]
async fn native_command_http_failures_settle_the_current_turn() {
    for (reply, expected_status) in [
        (NativeCommandReply::Http404, Some("404 Not Found")),
        (NativeCommandReply::Disconnect, None),
    ] {
        let mut wire = TurnWire::start_native_command(false, reply).await;
        wire.request("/command").await;
        let (status, error) = tokio::time::timeout(Duration::from_secs(5), async {
            let mut surfaced = None;
            loop {
                match wire.events.recv().await.unwrap().unwrap() {
                    AgentEvent::Error { message } => surfaced = Some(message),
                    AgentEvent::Done { status, error, .. } => return (status, error.or(surfaced)),
                    _ => {}
                }
            }
        })
        .await
        .unwrap();
        assert_eq!(status, DoneStatus::Errored);
        let error = error.expect("native command HTTP failure is surfaced");
        assert!(error.contains("opencode POST /session/fixture/command:"));
        match expected_status {
            Some(expected_status) => assert!(error.contains(expected_status), "{error}"),
            None => assert!(!error.contains("404 Not Found"), "{error}"),
        }
        assert!(
            !wire
                .posts
                .lock()
                .unwrap()
                .iter()
                .any(|(path, _)| path.ends_with("/prompt_async")),
            "failed native command must not fall back to an ordinary prompt"
        );
    }
}

#[tokio::test]
async fn late_native_command_failure_does_not_poison_the_queued_turn() {
    let mut wire = TurnWire::start_native_command(true, NativeCommandReply::DelayedHttp404).await;
    wire.requests_any_order(&["/command", "/abort"]).await;
    wire.status("busy");
    wire.status("idle");
    wire.request("/prompt_async").await;
    wire.command_failure_release
        .take()
        .unwrap()
        .send(())
        .unwrap();
    tokio::task::yield_now().await;
    wire.status("busy");
    wire.bus.send(json!({"type":"message.updated", "properties":{"info":{"id":"answer", "sessionID":"fixture", "role":"assistant"}}})).unwrap();
    wire.bus.send(json!({"type":"message.part.updated", "properties":{"part":{"id":"text", "messageID":"answer", "sessionID":"fixture", "type":"text", "text":"SECOND_OK"}}})).unwrap();
    wire.status("idle");
    let (status, text) = wire.done().await;
    assert_eq!(status, DoneStatus::Completed);
    assert_eq!(text, "SECOND_OK");
}

#[tokio::test]
async fn v2_wire_streams_text_and_settles_on_execution_success() {
    let mut wire = TurnWire::start_proto(false, true).await;
    wire.request("/api/model").await;
    wire.request("/prompt").await;
    wire.v2("session.execution.started", json!({"sessionID": "fixture"}));
    wire.v2(
        "session.step.started",
        json!({
            "sessionID": "fixture",
            "assistantMessageID": "msg_a",
            "model": {"id": "muse", "providerID": "opencode", "variant": "low"},
        }),
    );
    wire.v2(
        "session.text.started",
        json!({"sessionID": "fixture", "assistantMessageID": "msg_a", "ordinal": 0}),
    );
    wire.v2(
        "session.text.delta",
        json!({"sessionID": "fixture", "assistantMessageID": "msg_a", "ordinal": 0, "delta": "PONG"}),
    );
    wire.v2(
        "session.text.ended",
        json!({"sessionID": "fixture", "assistantMessageID": "msg_a", "ordinal": 0, "text": "PONG"}),
    );
    let step = |wire: &TurnWire, message: &str, model: &str, tokens: Value, cumulative: Value| {
        wire.v2(
            "session.step.started",
            json!({
                "sessionID": "fixture",
                "assistantMessageID": message,
                "model": {"id": model, "providerID": "opencode"},
            }),
        );
        wire.v2(
            "session.step.ended",
            json!({
                "sessionID": "fixture", "assistantMessageID": message,
                "finish": "stop", "cost": 0, "tokens": tokens,
            }),
        );
        wire.v2(
            "session.usage.updated",
            json!({"sessionID": "fixture", "cost": 0, "tokens": cumulative}),
        );
    };
    wire.v2(
        "session.step.ended",
        json!({
            "sessionID": "fixture", "assistantMessageID": "msg_a",
            "finish": "stop", "cost": 0,
            "tokens": {"input": 10, "output": 2, "reasoning": 0, "cache": {"read": 0, "write": 0}}
        }),
    );
    wire.v2(
        "session.usage.updated",
        json!({
            "sessionID": "fixture", "cost": 0,
            "tokens": {"input": 10, "output": 2, "reasoning": 0, "cache": {"read": 0, "write": 0}}
        }),
    );
    step(
        &wire,
        "msg_b",
        "long-context",
        json!({"input": 20, "output": 3, "reasoning": 0, "cache": {"read": 5, "write": 0}}),
        json!({"input": 30, "output": 5, "reasoning": 0, "cache": {"read": 5, "write": 0}}),
    );
    step(
        &wire,
        "msg_c",
        "long-context",
        json!({"input": 0, "output": 0, "reasoning": 0, "cache": {"read": 0, "write": 0}}),
        json!({"input": 30, "output": 5, "reasoning": 0, "cache": {"read": 5, "write": 0}}),
    );
    step(
        &wire,
        "msg_d",
        "long-context",
        json!({"input": 4, "output": 1, "reasoning": 0, "cache": {"read": 0, "write": 0}}),
        json!({"input": 34, "output": 6, "reasoning": 0, "cache": {"read": 5, "write": 0}}),
    );
    wire.v2(
        "session.execution.succeeded",
        json!({"sessionID": "fixture"}),
    );

    let (status, text, usage, context_usage) =
        tokio::time::timeout(Duration::from_secs(5), async {
            let mut text = String::new();
            let mut usage = None;
            let mut context_usage = Vec::new();
            loop {
                match wire.events.recv().await.unwrap().unwrap() {
                    AgentEvent::TextDelta { text: delta } => text.push_str(&delta),
                    AgentEvent::Usage {
                        input_tokens,
                        output_tokens,
                    } => usage = Some((input_tokens, output_tokens)),
                    AgentEvent::ContextUsage { tokens, window } => {
                        context_usage.push((tokens, window))
                    }
                    AgentEvent::Done { status, .. } => {
                        return (status, text, usage, context_usage);
                    }
                    _ => {}
                }
            }
        })
        .await
        .unwrap();
    assert_eq!(status, DoneStatus::Completed);
    assert_eq!(text, "PONG");
    assert_eq!(usage, Some((4, 1)));
    assert_eq!(
        context_usage,
        vec![
            (Some(12), Some(1000)),
            (Some(28), Some(2000)),
            (Some(5), Some(2000)),
        ]
    );
}

#[tokio::test]
async fn v2_wire_tool_frames_open_and_resolve_chips() {
    let mut wire = TurnWire::start_proto(false, true).await;
    wire.request("/api/model").await;
    wire.request("/prompt").await;
    wire.v2("session.execution.started", json!({"sessionID": "fixture"}));
    wire.v2(
        "session.step.started",
        json!({"sessionID": "fixture", "assistantMessageID": "msg_a"}),
    );
    wire.v2(
        "session.tool.input.started",
        json!({"sessionID": "fixture", "assistantMessageID": "msg_a", "id": "call_1", "name": "read"}),
    );
    wire.v2(
        "session.tool.called",
        json!({
            "sessionID": "fixture", "assistantMessageID": "msg_a",
            "id": "call_1", "input": {"path": "/tmp/oc2x-probe/note.txt"}
        }),
    );
    wire.v2(
        "session.tool.success",
        json!({
            "sessionID": "fixture", "assistantMessageID": "msg_a",
            "id": "call_1",
            "content": [{"type": "text", "text": "1: The secret word is BANANA42"}]
        }),
    );
    wire.v2(
        "session.execution.succeeded",
        json!({"sessionID": "fixture"}),
    );

    let (status, calls, results) = tokio::time::timeout(Duration::from_secs(5), async {
        let mut calls = Vec::new();
        let mut results = Vec::new();
        loop {
            match wire.events.recv().await.unwrap().unwrap() {
                AgentEvent::ToolCall { id, call } => calls.push((id, call)),
                AgentEvent::ToolResult { id, output, .. } => results.push((id, output)),
                AgentEvent::Done { status, .. } => return (status, calls, results),
                _ => {}
            }
        }
    })
    .await
    .unwrap();
    assert_eq!(status, DoneStatus::Completed);
    assert_eq!(
        calls,
        vec![(
            "fixture:msg_a:call_1".to_owned(),
            ToolCall::ReadFile {
                path: "/tmp/oc2x-probe/note.txt".to_owned()
            }
        )]
    );
    assert_eq!(
        results,
        vec![(
            "fixture:msg_a:call_1".to_owned(),
            Some("1: The secret word is BANANA42".to_owned())
        )]
    );
}

#[tokio::test]
async fn v2_execution_failure_and_interrupt_settle_the_turn() {
    let mut wire = TurnWire::start_proto(false, true).await;
    wire.request("/api/model").await;
    wire.request("/prompt").await;
    wire.v2("session.execution.started", json!({"sessionID": "fixture"}));
    wire.v2(
        "session.execution.failed",
        json!({"sessionID": "fixture", "error": {"type": "provider.auth", "message": ""}}),
    );
    let (status, error) = tokio::time::timeout(Duration::from_secs(5), async {
        let mut error = None;
        loop {
            match wire.events.recv().await.unwrap().unwrap() {
                AgentEvent::Error { message } => error = Some(message),
                AgentEvent::Done {
                    status, error: e, ..
                } => return (status, e.or(error)),
                _ => {}
            }
        }
    })
    .await
    .unwrap();
    assert_eq!(status, DoneStatus::Errored);
    assert!(error.unwrap().contains("provider.auth"));

    let mut wire = TurnWire::start_proto(false, true).await;
    wire.request("/api/model").await;
    wire.request("/prompt").await;
    wire.v2("session.execution.started", json!({"sessionID": "fixture"}));
    wire.interrupt.cancel();
    wire.request("/interrupt").await;
    wire.v2(
        "session.execution.interrupted",
        json!({"sessionID": "fixture", "reason": "user"}),
    );
    assert_eq!(wire.done().await.0, DoneStatus::Interrupted);
}

async fn read_http_request_headers(socket: &mut tokio::net::TcpStream) {
    use tokio::io::AsyncReadExt;
    let mut headers = Vec::new();
    while !headers.ends_with(b"\r\n\r\n") {
        headers.push(socket.read_u8().await.unwrap());
        assert!(headers.len() <= 8192, "unexpectedly large request headers");
    }
}

#[test]
#[allow(
    clippy::disallowed_methods,
    reason = "a synchronous test re-running itself in a child process"
)]
fn http_client_never_proxies_the_loopback_server() {
    const NAME: &str = "opencode::tests::http_client_never_proxies_the_loopback_server";
    const CHILD: &str = "WU_OPENCODE_PROXY_PROBE";
    if std::env::var_os(CHILD).is_none() {
        // reqwest reads the proxy from the environment, so the body runs in a child process.
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args([NAME, "--exact", "--test-threads=1"])
            .env(CHILD, "1")
            .env("HTTP_PROXY", "http://127.0.0.1:1")
            .env("http_proxy", "http://127.0.0.1:1")
            .env_remove("NO_PROXY")
            .env_remove("no_proxy")
            .env_remove("REQUEST_METHOD")
            .output()
            .unwrap();
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(
            output.status.success() && stdout.contains("1 passed"),
            "child probe failed:\n{stdout}\n{}",
            String::from_utf8_lossy(&output.stderr)
        );
        return;
    }
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    runtime.block_on(async {
        use tokio::io::AsyncWriteExt;
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/global/health", listener.local_addr().unwrap());
        tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            read_http_request_headers(&mut socket).await;
            socket
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\n\r\n")
                .await
                .unwrap();
        });
        assert!(reqwest::Client::new().get(&url).send().await.is_err());
        let response = http_client()
            .unwrap()
            .get(&url)
            .send()
            .await
            .expect("opencode client must reach loopback directly");
        assert!(response.status().is_success());
    });
}

#[test]
fn provider_discovery_ignores_metadata_and_accepts_null_optional_fields() {
    let catalog: ProviderCatalog = serde_json::from_str(
        r#"{
        "all": [{
            "id": "local", "name": null,
            "models": {
                "small": {"name": null, "variants": null},
                "thinking": {
                    "variants": {"high": {"nested": [{"unused": "configuration"}]}},
                    "capabilities": {"large": [1, 2, 3]},
                    "cost": {"input": 1}, "limit": {"context": 200000}
                }
            },
            "options": {"unused": [true, false, null]}
        }, {"id": "empty", "models": null}],
        "connected": null,
        "default": {"unused": "model"}
    }"#,
    )
    .unwrap();
    let models = models_from_providers(&catalog);
    assert_eq!(models.len(), 2);
    assert_eq!(models[0].id, "local/small");
    assert_eq!(models[0].description.as_deref(), Some("local"));
    assert!(models[0].reasoning_levels.is_empty());
    assert_eq!(models[1].reasoning_levels, vec![ReasoningLevel::High]);
    assert_eq!(
        pick_variant(&catalog, "local", "thinking", Some(ReasoningLevel::High)).as_deref(),
        Some("high")
    );
}

#[test]
fn models_map_provider_catalog_with_variant_ladders() {
    let providers: ProviderCatalog = serde_json::from_value(json!({
        "all": [
            {
                "id": "anthropic",
                "name": "Anthropic",
                "models": {
                    "claude-opus-5": {
                        "name": "Claude Opus 5",
                        "variants": {"low": {}, "medium": {}, "high": {}, "max": {}},
                    },
                    "claude-haiku-4-5": {"name": "Claude Haiku 4.5"},
                }
            },
            {
                "id": "opencode",
                "name": "OpenCode Zen",
                "models": {"big-pickle": {"name": "Big Pickle"}}
            }
        ],
        "default": {},
        "connected": ["anthropic"],
    }))
    .unwrap();
    let models = models_from_providers(&providers);
    assert_eq!(models.len(), 2);
    let opus = models
        .iter()
        .find(|model| model.id == "anthropic/claude-opus-5")
        .unwrap();
    assert_eq!(opus.label, "Claude Opus 5");
    assert_eq!(opus.description.as_deref(), Some("Anthropic"));
    assert_eq!(
        opus.reasoning_levels,
        vec![
            ReasoningLevel::Low,
            ReasoningLevel::Medium,
            ReasoningLevel::High,
            ReasoningLevel::Max
        ]
    );
    let haiku = models
        .iter()
        .find(|model| model.id == "anthropic/claude-haiku-4-5")
        .unwrap();
    assert!(haiku.reasoning_levels.is_empty());
    assert!(
        !models.iter().any(|model| model.id == "opencode/big-pickle"),
        "unconnected providers stay out of the picker"
    );
}

#[test]
fn missing_connected_list_falls_back_to_the_full_catalog() {
    for connected in [None, Some(json!([]))] {
        let mut providers = json!({
            "all": [
                {"id": "a", "models": {"m1": {}}},
                {"id": "b", "models": {"m2": {}}},
            ],
        });
        if let Some(connected) = connected {
            providers["connected"] = connected;
        }
        let providers: ProviderCatalog = serde_json::from_value(providers).unwrap();
        assert_eq!(models_from_providers(&providers).len(), 2);
    }
}

#[test]
fn variants_only_ride_models_that_advertise_them() {
    let providers: ProviderCatalog = serde_json::from_value(json!({
        "all": [{
            "id": "anthropic",
            "models": {
                "opus": {"variants": {"high": {}, "max": {}}},
                "haiku": {},
            }
        }]
    }))
    .unwrap();
    assert_eq!(
        pick_variant(&providers, "anthropic", "opus", Some(ReasoningLevel::High)).as_deref(),
        Some("high")
    );
    assert_eq!(
        pick_variant(&providers, "anthropic", "opus", Some(ReasoningLevel::XHigh)).as_deref(),
        Some("high")
    );
    assert_eq!(
        pick_variant(&providers, "anthropic", "haiku", Some(ReasoningLevel::High)),
        None
    );
    assert_eq!(pick_variant(&providers, "anthropic", "opus", None), None);
    assert_eq!(
        pick_variant(&providers, "missing", "opus", Some(ReasoningLevel::Low)),
        None
    );
}

#[test]
fn prompt_body_carries_model_variant_and_attachments() {
    let body = prompt_body(
        "hello",
        Some(("anthropic", "claude-opus-5")),
        Some("high"),
        &["/tmp/shot.png".to_owned()],
    );
    assert_eq!(body["model"]["providerID"], "anthropic");
    assert_eq!(body["model"]["modelID"], "claude-opus-5");
    assert_eq!(body["variant"], "high");
    assert_eq!(body["parts"][0]["type"], "text");
    assert_eq!(body["parts"][0]["text"], "hello");
    assert_eq!(body["parts"][1]["type"], "file");
    assert_eq!(body["parts"][1]["mime"], "image/png");
    assert_eq!(body["parts"][1]["filename"], "shot.png");
    assert_eq!(body["parts"][1]["url"], "file:///tmp/shot.png");
}

fn feed_with_assistant(message: &str) -> SessionFeed {
    let mut feed = SessionFeed::default();
    feed.message_is_assistant.insert(message.into(), true);
    feed
}

#[test]
fn reasoning_parts_stream_as_reasoning_deltas() {
    let mut feed = feed_with_assistant("msg_a");
    let open = json!({
        "id": "prt_r", "messageID": "msg_a", "sessionID": "ses_1",
        "type": "reasoning", "text": "",
    });
    assert!(part_snapshot_events(&mut feed, &open, true, None).is_empty());
    let properties = json!({"sessionID": "ses_1", "messageID": "msg_a", "partID": "prt_r"});
    let events = part_delta_events(&mut feed, &properties, "prt_r", "thinking hard");
    assert!(matches!(
        events.as_slice(),
        [AgentEvent::ReasoningDelta { text }] if text == "thinking hard"
    ));
    let close = json!({
        "id": "prt_r", "messageID": "msg_a", "sessionID": "ses_1",
        "type": "reasoning", "text": "thinking hard",
    });
    assert!(part_snapshot_events(&mut feed, &close, true, None).is_empty());
    let more = json!({
        "id": "prt_r", "messageID": "msg_a", "sessionID": "ses_1",
        "type": "reasoning", "text": "thinking hard about it",
    });
    let events = part_snapshot_events(&mut feed, &more, true, None);
    assert!(matches!(
        events.as_slice(),
        [AgentEvent::ReasoningDelta { text }] if text == " about it"
    ));
}

#[test]
fn parts_ahead_of_their_message_role_are_held_and_replayed() {
    let mut feed = SessionFeed::default();
    let part = json!({
        "id": "prt_r", "messageID": "msg_a", "sessionID": "ses_1",
        "type": "reasoning", "text": "early thought",
    });
    let other = json!({
        "id": "prt_b", "messageID": "msg_b", "sessionID": "ses_1",
        "type": "text", "text": "later answer",
    });
    assert!(part_snapshot_events(&mut feed, &part, true, None).is_empty());
    assert!(part_snapshot_events(&mut feed, &other, true, None).is_empty());
    assert_eq!(feed.parts_awaiting_role.len(), 2);
    feed.message_is_assistant.insert("msg_a".into(), true);
    let mut turn = TurnState::begin(None);
    let events = replay_pending(&mut feed, "msg_a", true, &mut turn);
    assert!(matches!(
        events.as_slice(),
        [AgentEvent::ReasoningDelta { text }] if text == "early thought"
    ));
    assert!(turn.saw_content);
    feed.message_is_assistant.insert("msg_b".into(), true);
    let events = replay_pending(&mut feed, "msg_b", true, &mut turn);
    assert!(matches!(
        events.as_slice(),
        [AgentEvent::TextDelta { text }] if text == "later answer"
    ));
}

#[test]
fn main_feed_user_text_is_the_prompt_echo_and_never_renders() {
    let mut feed = SessionFeed::default();
    feed.message_is_assistant.insert("msg_u".into(), false);
    let part = json!({
        "id": "prt_u", "messageID": "msg_u", "sessionID": "ses_1",
        "type": "text", "text": "the prompt",
    });
    assert!(part_snapshot_events(&mut feed, &part, true, None).is_empty());
    let mut child = SessionFeed::default();
    child.message_is_assistant.insert("msg_u".into(), false);
    let events = part_snapshot_events(&mut child, &part, false, None);
    assert!(matches!(
        events.as_slice(),
        [AgentEvent::UserMessage { text }] if text == "the prompt"
    ));
    assert!(part_snapshot_events(&mut child, &part, false, None).is_empty());
}

#[test]
fn tool_parts_open_and_resolve_once() {
    let mut feed = feed_with_assistant("msg_a");
    let running = json!({
        "id": "prt_t", "messageID": "msg_a", "sessionID": "ses_1",
        "type": "tool", "tool": "bash", "callID": "call-1",
        "state": {"status": "running", "input": {"command": "echo ok"}},
    });
    let events = part_snapshot_events(&mut feed, &running, true, None);
    assert!(matches!(
        events.as_slice(),
        [AgentEvent::ToolCall { id, call: ToolCall::Exec { command } }]
            if id == "call-1" && command == "echo ok"
    ));
    assert!(part_snapshot_events(&mut feed, &running, true, None).is_empty());
    let done = json!({
        "id": "prt_t", "messageID": "msg_a", "sessionID": "ses_1",
        "type": "tool", "tool": "bash", "callID": "call-1",
        "state": {"status": "completed", "input": {"command": "echo ok"}, "output": "ok\n"},
    });
    let events = part_snapshot_events(&mut feed, &done, true, None);
    assert!(matches!(
        events.as_slice(),
        [AgentEvent::ToolResult { id, is_error: false, output: Some(output), .. }]
            if id == "call-1" && output == "ok\n"
    ));
}

#[test]
fn a_running_tool_without_arguments_opens_before_it_completes() {
    let mut feed = feed_with_assistant("msg_a");
    let pending = json!({
        "id": "prt_w", "messageID": "msg_a", "sessionID": "ses_1",
        "type": "tool", "tool": "slow_slow_wait", "callID": "call-w",
        "state": {"status": "pending", "input": {}},
    });
    assert!(part_snapshot_events(&mut feed, &pending, true, None).is_empty());
    let running = json!({
        "id": "prt_w", "messageID": "msg_a", "sessionID": "ses_1",
        "type": "tool", "tool": "slow_slow_wait", "callID": "call-w",
        "state": {"status": "running", "input": {}},
    });
    let events = part_snapshot_events(&mut feed, &running, true, None);
    assert!(
        matches!(events.as_slice(), [AgentEvent::ToolCall { id, .. }] if id == "call-w"),
        "{events:?}"
    );
}

#[test]
fn task_spawn_registers_child_by_metadata_and_completion_settles() {
    for name in ["task", "subagent"] {
        let mut feed = feed_with_assistant("msg_a");
        let mut children = HashMap::new();
        let mut pending = VecDeque::new();
        let mut unbound = HashMap::new();
        let running = json!({
            "id": "prt_task", "messageID": "msg_a", "sessionID": "ses_parent",
            "type": "tool", "tool": name,
            "state": {
                "status": "running",
                "input": {"description": "Scan crates", "prompt": "scan", "subagent_type": "general"},
                "metadata": {"sessionId": "ses_child", "parentSessionId": "ses_parent"},
            },
        });
        let events = part_snapshot_events(
            &mut feed,
            &running,
            true,
            Some((&mut children, &mut pending, &mut unbound)),
        );
        assert!(matches!(
            events.as_slice(),
            [AgentEvent::ToolCall { id, call: ToolCall::Unknown { name, .. } }]
                if id == "prt_task" && name == "Agent: Scan crates"
        ));
        assert_eq!(
            children.get("ses_child").unwrap().parent_tool_use_id,
            "prt_task"
        );
        let completed = json!({
            "id": "prt_task", "messageID": "msg_a", "sessionID": "ses_parent",
            "type": "tool", "tool": name,
            "state": {
                "status": "completed",
                "input": {"description": "Scan crates"},
                "output": "<task_result>done</task_result>",
                "metadata": {"sessionId": "ses_child"},
            },
        });
        assert_eq!(
            task_completion(&completed),
            Some(("ses_child".to_owned(), false))
        );
    }
}

#[test]
fn child_binding_falls_back_to_title_match() {
    let mut children = HashMap::new();
    let mut pending = VecDeque::new();
    pending.push_back(PendingSpawn {
        tool_part_id: "prt_1".into(),
        description: "Scan crates".into(),
    });
    pending.push_back(PendingSpawn {
        tool_part_id: "prt_2".into(),
        description: "Write docs".into(),
    });
    assert!(bind_child(
        &mut children,
        &mut pending,
        "ses_b",
        "Write docs (@general subagent)"
    ));
    assert_eq!(children.get("ses_b").unwrap().parent_tool_use_id, "prt_2");
    assert_eq!(pending.len(), 1);
    assert!(bind_child(&mut children, &mut pending, "ses_a", "mystery"));
    assert_eq!(children.get("ses_a").unwrap().parent_tool_use_id, "prt_1");
    assert!(!bind_child(
        &mut children,
        &mut pending,
        "ses_c",
        "anything"
    ));
}

#[test]
fn questions_map_to_input_panel_shape() {
    let properties = json!({
        "id": "que_1",
        "sessionID": "ses_1",
        "questions": [{
            "question": "Which color?",
            "header": "Color",
            "options": [
                {"label": "Red", "description": "warm"},
                {"label": "Blue", "description": "cool"},
            ],
            "multiple": true,
        }],
    });
    let questions = map_questions(&properties);
    assert_eq!(questions.len(), 1);
    assert_eq!(questions[0].id, "q0");
    assert_eq!(questions[0].header, "Color");
    assert_eq!(questions[0].question, "Which color?");
    assert_eq!(questions[0].options, vec!["Red", "Blue"]);
    assert!(questions[0].multi_select);
}

#[test]
fn tool_names_type_the_common_calls() {
    assert_eq!(
        tool_call("bash", &json!({"command": "ls -la"})),
        ToolCall::Exec {
            command: "ls -la".into()
        }
    );
    assert_eq!(
        tool_call(
            "edit",
            &json!({"filePath": "/w/a.rs", "oldString": "a", "newString": "b"}),
        ),
        ToolCall::EditFile {
            path: "/w/a.rs".into(),
            old_string: Some("a".into()),
            new_string: Some("b".into()),
        }
    );
    let call = tool_call("task", &json!({"description": "Scan crates"}));
    assert!(matches!(&call, ToolCall::Unknown { name, .. } if name == "Agent: Scan crates"));
    assert!(call.is_subagent_spawn());
    assert_eq!(
        tool_call(
            "todowrite",
            &json!({"todos": [
                {"content": "step one", "status": "completed"},
                {"content": "step two", "status": "in_progress"},
                {"content": "step three", "status": "pending"},
                {"content": "step four", "status": "cancelled"},
            ]}),
        ),
        ToolCall::Todo {
            items: vec![
                TodoItem::new("step one", TodoStatus::Completed),
                TodoItem::new("step two", TodoStatus::InProgress),
                TodoItem::new("step three", TodoStatus::Pending),
                TodoItem::new("step four", TodoStatus::Pending),
            ]
        }
    );
    let call = tool_call("mystery", &json!({"x": 1}));
    assert!(matches!(&call, ToolCall::Unknown { name, input: Some(_) } if name == "mystery"));
    assert!(!call.is_subagent_spawn());
}

#[test]
fn commands_and_skills_split_from_the_wire() {
    let wire = json!([
        {"name": "init", "description": "Create AGENTS.md", "source": "command"},
        {"name": "share"},
        {"description": "nameless is dropped"},
        {"name": "review", "description": "Native skill", "source": "skill"},
    ]);
    let commands = commands_from_wire(&wire);
    assert_eq!(
        commands
            .iter()
            .map(|command| command.name.as_str())
            .collect::<Vec<_>>(),
        vec!["init", "share"]
    );
    assert_eq!(commands[0].description, "Create AGENTS.md");
    assert_eq!(command_names(&wire), vec!["init", "share", "review"]);
    let skills = skills_from_wire(&wire);
    assert_eq!(skills.len(), 1);
    assert_eq!(skills[0].name, "review");
    assert_eq!(skills[0].description, "Native skill");
    assert_eq!(skills[0].path.as_deref(), Some("opencode-skill:review"));
}

#[test]
fn skill_catalog_rejects_unrepresentable_names() {
    let mut wire: Vec<Value> = [
        "",
        "two words",
        " padded",
        "line\nbreak",
        "tab\tname",
        "nul\0name",
        "non\u{a0}breaking",
        "review[ui]",
        "review/extra",
    ]
    .into_iter()
    .map(|name| json!({"name": name, "source": "skill"}))
    .collect();
    wire.push(json!({"name": "审查-é:ui.v2_test", "source": "skill"}));
    let skills = skills_from_wire(&json!(wire));
    assert_eq!(skills.len(), 1);
    assert_eq!(
        skills[0].path.as_deref(),
        Some("opencode-skill:审查-é:ui.v2_test")
    );
}

#[test]
fn skills_plan_a_native_command_or_a_load_instruction() {
    let skill = |name: &str| SkillRef {
        name: name.into(),
        path: format!("opencode-skill:{name}"),
    };
    let review = [skill("review")];
    assert_eq!(
        plan_prompt("\n  $review inspect tests", &review, &[]),
        ("/review inspect tests".to_owned(), true)
    );
    assert_eq!(
        plan_prompt("$review", &review, &[]),
        ("/review".to_owned(), true)
    );
    assert_eq!(
        plan_prompt("no skills", &[], &[]),
        ("no skills".to_owned(), false)
    );
    let instruction = "\n\nBefore you start, load these skills with the `skill` tool:";
    for (prompt, skills, attachments) in [
        ("please $review this", review.to_vec(), Vec::new()),
        (
            "Continue this conversation.\n\n$review it",
            review.to_vec(),
            Vec::new(),
        ),
        (
            "$review this",
            review.to_vec(),
            vec!["/tmp/shot.png".to_owned()],
        ),
        (
            "$review and $docs",
            vec![skill("review"), skill("docs"), skill("review")],
            Vec::new(),
        ),
    ] {
        let (planned, native) = plan_prompt(prompt, &skills, &attachments);
        assert!(!native, "{prompt}");
        assert!(planned.starts_with(prompt), "{planned}");
        assert!(planned.contains(instruction), "{planned}");
        assert!(planned.contains("`review`"), "{planned}");
    }
    let (planned, _) = plan_prompt("$review and $docs", &[skill("review"), skill("docs")], &[]);
    assert!(planned.ends_with("`review`, `docs`."), "{planned}");
}

#[test]
fn one_x_commands_carry_the_chosen_model_and_variant() {
    let model = ("anthropic".to_owned(), "opus".to_owned());
    assert_eq!(
        command_body_v1("review", "now", Some(&model), Some("high")),
        json!({"command": "review", "arguments": "now", "model": "anthropic/opus", "variant": "high"})
    );
    assert_eq!(
        command_body_v1("init", "", None, None),
        json!({"command": "init", "arguments": ""})
    );
}

#[test]
fn selected_command_removed_after_discovery_is_not_downgraded_to_prompt_text() {
    let live = command_names(&json!([]));
    let error = native_command_request("/project-review", &live, true).unwrap_err();
    assert_eq!(
        error.to_string(),
        "harness protocol error: The selected OpenCode command /project-review is no longer available in this project"
    );
    assert!(
        native_command_request("/project-review", &live, false)
            .unwrap()
            .is_none()
    );
    let live = command_names(&json!([{"name": "project-review"}]));
    assert_eq!(
        native_command_request("/project-review now", &live, false).unwrap(),
        Some(("project-review", "now"))
    );
}

#[test]
fn stall_and_startup_defaults() {
    if std::env::var_os(STALL_ENV).is_none() {
        assert_eq!(stall_bound(), Some(DEFAULT_STALL_BOUND));
    }
    if std::env::var_os(server::STARTUP_TIMEOUT_ENV).is_none() {
        assert_eq!(startup_timeout(), DEFAULT_STARTUP_TIMEOUT);
    }
}

#[test]
fn directory_header_percent_encodes() {
    assert_eq!(
        encode_directory("/home/u/my project"),
        "/home/u/my%20project"
    );
    assert_eq!(encode_directory("/plain/path"), "/plain/path");
}

#[test]
fn v2_frames_normalize_to_v1_payloads() {
    let mut tools = HashMap::new();
    let out = normalize_v2_frame(
        json!({"id":"evt_1","type":"session.execution.started","data":{"sessionID":"ses_1"}}),
        &mut tools,
    );
    assert_eq!(
        out,
        vec![json!({"type":"session.status","properties":{
            "sessionID":"ses_1","status":{"type":"busy"}}})]
    );
    for kind in [
        "session.execution.succeeded",
        "session.execution.interrupted",
    ] {
        let out = normalize_v2_frame(
            json!({"id":"evt_2","type":kind,"data":{"sessionID":"ses_1"}}),
            &mut tools,
        );
        let expected = if kind == "session.execution.interrupted" {
            "session.interrupted"
        } else {
            "session.idle"
        };
        assert_eq!(
            out,
            vec![json!({"type": expected, "properties":{"sessionID":"ses_1"}})]
        );
    }
    let out = normalize_v2_frame(
        json!({"id":"evt_3","type":"session.execution.failed","data":{
            "sessionID":"ses_1","error":{"type":"provider.auth","message":""}}}),
        &mut tools,
    );
    assert_eq!(out.len(), 2);
    assert_eq!(
        out[0],
        json!({"type":"session.error","properties":{
            "sessionID":"ses_1",
            "error":{"name":"provider.auth","data":{"message":"provider.auth"}}}}),
    );
    assert!(
        normalize_v2_frame(
            json!({"id":"evt_4","type":"session.step.failed","data":{
            "sessionID":"ses_1","error":{"type":"aborted","message":"Step interrupted"}}}),
            &mut tools,
        )
        .is_empty()
    );
    let out = normalize_v2_frame(
        json!({"id":"evt_5","type":"session.step.started","data":{
            "sessionID":"ses_1","assistantMessageID":"msg_a"}}),
        &mut tools,
    );
    assert_eq!(
        out,
        vec![json!({"type":"message.updated","properties":{
            "info":{"sessionID":"ses_1","id":"msg_a","role":"assistant"}}})]
    );
    let out = normalize_v2_frame(
        json!({"id":"evt_6","type":"session.text.delta","data":{
            "sessionID":"ses_1","assistantMessageID":"msg_a","ordinal":0,"delta":"hi"}}),
        &mut tools,
    );
    assert_eq!(
        out,
        vec![json!({"type":"message.part.delta","properties":{
            "sessionID":"ses_1","messageID":"msg_a","partID":"msg_a:t0",
            "field":"text","delta":"hi"}})]
    );
    normalize_v2_frame(
        json!({"id":"evt_7","type":"session.tool.input.started","data":{
            "sessionID":"ses_1","assistantMessageID":"msg_a","id":"call_1","name":"read"}}),
        &mut tools,
    );
    let out = normalize_v2_frame(
        json!({"id":"evt_8","type":"session.tool.called","data":{
            "sessionID":"ses_1","assistantMessageID":"msg_a","id":"call_1",
            "input":{"path":"/tmp/x"}}}),
        &mut tools,
    );
    assert_eq!(
        out,
        vec![json!({"type":"message.part.updated","properties":{"part":{
            "sessionID":"ses_1","messageID":"msg_a","id":"ses_1:msg_a:call_1","callID":"ses_1:msg_a:call_1",
            "type":"tool","tool":"read",
            "state":{"status":"running","input":{"path":"/tmp/x"}}}}})]
    );
    let out = normalize_v2_frame(
        json!({"id":"evt_9","type":"session.step.ended","data":{
            "sessionID":"ses_1","assistantMessageID":"msg_a","finish":"stop","cost":0,
            "tokens":{"input":10,"output":2,"reasoning":0,"cache":{"read":0,"write":0}}}}),
        &mut tools,
    );
    assert_eq!(
        out,
        vec![json!({"type":"message.updated","properties":{
            "info":{"sessionID":"ses_1","id":"usage","role":"assistant",
                    "tokens":{"input":10,"output":2,"reasoning":0,
                              "cache":{"read":0,"write":0}}}}})]
    );
    for data in [
        json!({"id":"evt_10","type":"session.usage.updated","data":{
            "sessionID":"ses_1","cost":0,"tokens":{"input":10,"output":2}}}),
        json!({"id":"evt_11","type":"session.step.ended","data":{
            "sessionID":"ses_1","assistantMessageID":"msg_a","tokens":"12"}}),
        json!({"id":"evt_11","type":"session.step.ended","data":{
            "sessionID":"ses_1","assistantMessageID":"msg_a"}}),
    ] {
        assert_eq!(normalize_v2_frame(data, &mut tools), Vec::<Value>::new());
    }
    let mut session_models = HashMap::new();
    let step = normalize_v2_frame_with_session_models(
        json!({"id":"evt_12","type":"session.step.started","data":{
            "sessionID":"ses_2","assistantMessageID":"msg_b",
            "model":{"id":"long-context","providerID":"opencode"}}}),
        &mut tools,
        &mut session_models,
    );
    assert_eq!(step.len(), 1);
    let out = normalize_v2_frame_with_session_models(
        json!({"id":"evt_13","type":"session.step.ended","data":{
            "sessionID":"ses_2","assistantMessageID":"msg_b","finish":"stop","cost":0,
            "tokens":{"input":1,"output":2}}}),
        &mut tools,
        &mut session_models,
    );
    assert_eq!(
        out,
        vec![json!({"type":"message.updated","properties":{
            "info":{"sessionID":"ses_2","id":"usage","role":"assistant",
                    "tokens":{"input":1,"output":2},
                    "providerID":"opencode","modelID":"long-context"}}})]
    );
    for model in [
        json!(null),
        json!("opencode/muse"),
        json!({"id":"", "providerID":"opencode"}),
    ] {
        normalize_v2_frame_with_session_models(
            json!({"id":"evt_14","type":"session.step.started","data":{
                "sessionID":"ses_2","assistantMessageID":"msg_c","model":model}}),
            &mut tools,
            &mut session_models,
        );
    }
    assert_eq!(
        session_models
            .get("ses_2")
            .map(|model| model.model_id.as_str()),
        Some("long-context")
    );
    let out = normalize_v2_frame(
        json!({"id":"evt_11","type":"permission.asked","data":{
            "id":"per_1","sessionID":"ses_1","action":"external_directory",
            "resources":["/tmp/*"]}}),
        &mut tools,
    );
    assert_eq!(
        out,
        vec![json!({"type":"permission.asked","properties":{
            "id":"per_1","sessionID":"ses_1","action":"external_directory","resources":["/tmp/*"]}})]
    );
    assert!(
        normalize_v2_frame(
            json!({"id":"evt_10","type":"catalog.updated","data":{}}),
            &mut tools,
        )
        .is_empty()
    );
}

#[test]
fn v2_model_list_folds_into_provider_catalog() {
    let list: V2ModelList = serde_json::from_value(json!({
        "location": {"directory": "/w"},
        "data": [
            {"providerID": "opencode", "id": "muse-spark", "name": "Muse Spark",
             "limit": {"context": 1000000},
             "variants": [{"id": "low", "settings": {"x": 1}}, {"id": "high"}],
             "enabled": true},
            {"providerID": "opencode", "id": "plain", "enabled": true},
            {"providerID": "dead", "id": "off", "enabled": false},
        ]
    }))
    .unwrap();
    let catalog = catalog_from_v2_models(list.data);
    let models = models_from_providers(&catalog);
    assert_eq!(models.len(), 2);
    let muse = models
        .iter()
        .find(|model| model.id == "opencode/muse-spark")
        .unwrap();
    assert_eq!(muse.label, "Muse Spark");
    assert_eq!(
        muse.reasoning_levels,
        vec![ReasoningLevel::Low, ReasoningLevel::High]
    );
    assert_eq!(
        pick_variant(
            &catalog,
            "opencode",
            "muse-spark",
            Some(ReasoningLevel::High)
        )
        .as_deref(),
        Some("high")
    );
    assert!(models.iter().all(|model| model.id != "dead/off"));
}

#[test]
fn prompt_body_v2_carries_text_and_files_only() {
    let body = prompt_body_v2("hello", &["/tmp/shot.png".to_owned()]);
    assert_eq!(body["text"], "hello");
    assert_eq!(body["files"][0]["uri"], "file:///tmp/shot.png");
    assert_eq!(body["files"][0]["name"], "shot.png");
    assert!(body.get("model").is_none());
    assert!(body.get("parts").is_none());
}

#[test]
fn session_rules_force_asks_without_unlocking_denies() {
    let agent = json!([
        {"permission": "*", "pattern": "*", "action": "allow"},
        {"permission": "edit", "pattern": "*", "action": "deny"},
        {"permission": "bash", "pattern": "*", "action": "allow"},
        {"permission": "bash", "pattern": "rm *", "action": "deny"},
        {"permission": "bash", "pattern": "git *", "action": "allow"},
        {"permission": "webfetch", "pattern": "*", "action": "ask"},
        {"permission": "read", "pattern": "*.env", "action": "ask"},
    ]);
    let rules = session_rules(&agent, forced_asks(PermissionMode::Ask));
    assert_eq!(
        rules,
        vec![
            json!({"permission": "edit", "pattern": "*", "action": "deny"}),
            json!({"permission": "bash", "pattern": "*", "action": "ask"}),
            json!({"permission": "bash", "pattern": "rm *", "action": "deny"}),
            json!({"permission": "bash", "pattern": "git *", "action": "ask"}),
            json!({"permission": "webfetch", "pattern": "*", "action": "ask"}),
            json!({"permission": "task", "pattern": "*", "action": "ask"}),
        ]
    );
    for rule in &rules {
        assert_ne!(rule["action"], "allow", "{rule}");
    }
    let accept_edits = session_rules(&agent, forced_asks(PermissionMode::AcceptEdits));
    assert_eq!(accept_edits[0]["action"], "deny");
    assert_eq!(
        accept_edits[1],
        json!({"permission": "bash", "pattern": "*", "action": "ask"})
    );
    let restated = session_rules(&agent, forced_asks(PermissionMode::Auto));
    assert!(restated.contains(&json!({"permission": "task", "pattern": "*", "action": "allow"})));
    assert!(
        restated.contains(&json!({"permission": "bash", "pattern": "git *", "action": "allow"}))
    );
}

#[test]
fn session_rules_drop_denies_a_later_catch_all_overrides() {
    // A config ending in `"*": "allow"` really allows edits; restating its dead deny would leak into subagents.
    let agent = json!([
        {"permission": "edit", "pattern": "*", "action": "deny"},
        {"permission": "*", "pattern": "*", "action": "allow"},
    ]);
    let rules = session_rules(&agent, forced_asks(PermissionMode::Ask));
    assert_eq!(
        rules,
        vec![
            json!({"permission": "edit", "pattern": "*", "action": "ask"}),
            json!({"permission": "bash", "pattern": "*", "action": "ask"}),
            json!({"permission": "webfetch", "pattern": "*", "action": "ask"}),
            json!({"permission": "task", "pattern": "*", "action": "ask"}),
        ]
    );
}

#[test]
fn wildcards_match_like_opencode() {
    assert!(wildcard_matches("*", "edit"));
    assert!(wildcard_matches("ed*", "edit"));
    assert!(wildcard_matches("e?it", "edit"));
    assert!(wildcard_matches("*t", "edit"));
    assert!(!wildcard_matches("bash", "edit"));
    assert!(!wildcard_matches("edit?", "edit"));
}

#[test]
fn modes_decide_which_asks_wu_answers_itself() {
    assert!(auto_approves(PermissionMode::FullAccess, "bash"));
    assert!(auto_approves(PermissionMode::AcceptEdits, "edit"));
    assert!(!auto_approves(PermissionMode::AcceptEdits, "bash"));
    assert!(!auto_approves(PermissionMode::Ask, "edit"));
    assert!(!auto_approves(PermissionMode::Auto, "edit"));
}

#[test]
fn the_primary_agent_supplies_the_rules() {
    let agents = json!([
        {"name": "explore", "mode": "subagent", "permission": [{"permission": "*", "pattern": "*", "action": "deny"}]},
        {"name": "title", "mode": "primary", "hidden": true, "permission": []},
        {"name": "build", "mode": "primary", "permission": [{"permission": "*", "pattern": "*", "action": "allow"}]},
        {"name": "plan", "mode": "primary", "permission": [{"permission": "edit", "pattern": "*", "action": "deny"}]},
    ]);
    assert_eq!(
        primary_agent_rules(&agents, None).unwrap()[0]["action"],
        "allow"
    );
    assert_eq!(
        primary_agent_rules(&agents, Some("plan")).unwrap()[0]["action"],
        "deny"
    );
    assert!(primary_agent_rules(&agents, Some("missing")).is_none());
    assert!(primary_agent_rules(&json!({}), None).is_none());
}

#[test]
fn default_model_follows_opencodes_own_choice() {
    let providers: ProviderCatalog = serde_json::from_value(json!({
        "all": [
            {"id": "anthropic", "models": {"opus": {}, "sonnet": {}}},
            {"id": "opencode", "models": {"big-pickle": {}}},
        ],
        "connected": ["opencode", "anthropic"],
        "default": {"anthropic": "sonnet", "opencode": "big-pickle", "broken": 5},
    }))
    .unwrap();
    let models = models_from_providers(&providers);
    let pick = |configured: Option<&str>, recent: &[&str]| {
        let recent: Vec<String> = recent.iter().map(|id| id.to_string()).collect();
        default_model_id(&providers, configured, &recent, &models)
    };
    assert_eq!(
        pick(Some("anthropic/opus"), &["anthropic/sonnet"]).as_deref(),
        Some("anthropic/opus")
    );
    assert_eq!(
        pick(Some("gone/model"), &["gone/x", "anthropic/sonnet"]).as_deref(),
        Some("anthropic/sonnet")
    );
    assert_eq!(pick(None, &[]).as_deref(), Some("opencode/big-pickle"));
}

#[test]
fn permission_questions_describe_the_request() {
    let question = permission_question(&json!({
        "id": "per_1", "sessionID": "ses", "permission": "bash",
        "patterns": ["rm -rf build"], "metadata": {"command": "rm -rf build"},
    }));
    assert_eq!(question.header, crate::claude::PERMISSION_HEADER);
    assert_eq!(
        question.options,
        vec![
            crate::claude::PERMISSION_ALLOW,
            crate::claude::PERMISSION_DENY
        ]
    );
    assert_eq!(question.question, "Run `rm -rf build`?");
    for (properties, expected) in [
        (
            json!({"permission": "edit", "patterns": ["src/a.rs"], "metadata": {"filepath": "/w/src/a.rs"}}),
            "Edit /w/src/a.rs?",
        ),
        (
            json!({"permission": "webfetch", "patterns": ["https://example.com"]}),
            "Fetch https://example.com?",
        ),
        (
            json!({"action": "external_directory", "resources": ["/tmp/*"]}),
            "Access /tmp/* outside the project?",
        ),
        (
            json!({"permission": "lsp", "patterns": ["a", "b"]}),
            "Allow lsp for a, b?",
        ),
        (json!({"permission": "doom_loop"}), "Allow doom_loop?"),
        (
            json!({"permission": "task", "patterns": ["general"]}),
            "Start a general subagent?",
        ),
    ] {
        assert_eq!(permission_question(&properties).question, expected);
    }
}

#[tokio::test]
async fn full_access_permissions_stay_session_scoped_and_never_persist_grants() {
    for v2 in [false, true] {
        let mut wire = TurnWire::start_proto(false, v2).await;
        if v2 {
            wire.request("/api/model").await;
        }
        wire.request(if v2 { "/prompt" } else { "/prompt_async" })
            .await;
        let ask = |owner: Option<&str>, id: &str| {
            let mut data =
                json!({"id":id, "action":"external_directory", "resources":["/private/*"]});
            if let Some(owner) = owner {
                data["sessionID"] = json!(owner);
            }
            if v2 {
                json!({"type":"permission.asked", "data":data})
            } else {
                json!({"type":"permission.asked", "properties":data})
            }
        };
        wire.bus.send(ask(Some("foreign"), "foreign")).unwrap();
        wire.bus.send(ask(None, "missing")).unwrap();
        wire.bus.send(if v2 {
            json!({"type":"session.created","data":{"sessionID":"child","parentID":"fixture"}})
        } else {
            json!({"type":"session.created","properties":{"info":{"id":"child","parentID":"fixture"}}})
        }).unwrap();
        wire.bus.send(ask(Some("child"), "child")).unwrap();
        wire.bus.send(ask(Some("fixture"), "own")).unwrap();
        tokio::time::timeout(Duration::from_secs(2), async {
            while wire
                .posts
                .lock()
                .unwrap()
                .iter()
                .filter(|(path, _)| path.contains("permission"))
                .count()
                < 2
            {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        tokio::time::sleep(Duration::from_millis(50)).await;
        let posts = wire.posts.lock().unwrap();
        let approvals: Vec<_> = posts
            .iter()
            .filter(|(path, _)| path.contains("permission"))
            .collect();
        assert_eq!(
            approvals.len(),
            2,
            "foreign or ownerless permission was answered"
        );
        for (path, body) in approvals {
            assert_eq!(body["reply"], "once");
            assert!(!path.contains("foreign") && !path.contains("missing"));
        }
    }
}

#[tokio::test]
async fn full_access_approves_without_user_input_on_every_v2_version() {
    for version in ["2.0.0", "2.0.3", "2.0.4", "2.0.11"] {
        let mut wire = TurnWire::start_config(true, version, json!({})).await;
        wire.request("/api/model").await;
        wire.request("/prompt").await;
        let key = if version == "2.0.0" || version == "2.0.3" {
            "reply"
        } else {
            "decision"
        };
        for id in ["first", "second", "third"] {
            wire.v2("permission.asked", json!({"id":id, "sessionID":"fixture"}));
            let body = wire.posted(&format!("/permission/{id}/reply")).await;
            assert_eq!(body, json!({key: "once"}));
        }
    }
}

#[tokio::test]
async fn asking_modes_surface_permissions_and_reply_once_or_reject() {
    for (v2, version) in [(false, "1.18.20"), (true, "2.0.3"), (true, "2.0.11")] {
        for permission in ["ask", "accept_edits", "auto"] {
            for allow in [true, false] {
                let mut wire = TurnWire::start_fixture(Fixture {
                    v2,
                    version,
                    permission,
                    answer: Some(allow),
                    ..Fixture::default()
                })
                .await;
                if v2 {
                    wire.request("/api/model").await;
                }
                wire.request(if v2 { "/prompt" } else { "/prompt_async" })
                    .await;
                let data = json!({
                    "id": "per_1", "sessionID": "fixture", "permission": "bash",
                    "patterns": ["make"], "metadata": {"command": "make"},
                });
                if v2 {
                    wire.v2("permission.asked", data);
                } else {
                    wire.bus
                        .send(json!({"type": "permission.asked", "properties": data}))
                        .unwrap();
                }
                let (request_id, questions) = loop {
                    if let AgentEvent::InputRequested {
                        request_id,
                        questions,
                    } = wire.next_event().await
                    {
                        break (request_id, questions);
                    }
                };
                assert_eq!(request_id, "per_1");
                assert_eq!(questions.len(), 1);
                assert_eq!(questions[0].header, crate::claude::PERMISSION_HEADER);
                assert_eq!(questions[0].question, "Run `make`?");
                let resolved = loop {
                    if let AgentEvent::InputResolved { request_id } = wire.next_event().await {
                        break request_id;
                    }
                };
                assert_eq!(resolved, "per_1");
                let reply = if allow { "once" } else { "reject" };
                let (path, expected) = match (v2, version) {
                    (false, _) => ("/permission/per_1/reply", json!({"reply": reply})),
                    (true, "2.0.3") => (
                        "/api/session/fixture/permission/per_1/reply",
                        json!({"reply": reply}),
                    ),
                    _ => (
                        "/api/session/fixture/permission/per_1/reply",
                        json!({"decision": reply}),
                    ),
                };
                let posts = wire.posts.lock().unwrap();
                let replies: Vec<_> = posts
                    .iter()
                    .filter(|(posted, _)| posted.contains("permission"))
                    .collect();
                assert_eq!(replies.len(), 1, "{permission} {version}: {replies:?}");
                assert_eq!(replies[0].0, path);
                assert_eq!(replies[0].1, expected);
            }
        }
    }
}

#[tokio::test]
async fn permissions_do_not_auto_answer_agent_questions() {
    let mut wire = TurnWire::start_fixture(Fixture {
        permission: "full_access",
        answer: Some(false),
        ..Fixture::default()
    })
    .await;
    wire.request("/prompt_async").await;
    wire.bus.send(json!({"type": "question.asked", "properties": {
        "id": "question", "sessionID": "fixture", "questions": [{
            "header": "Choice", "question": "Continue?", "options": [{"label": "No"}, {"label": "Yes"}]
        }]
    }})).unwrap();
    let body = wire.posted("/question/question/reply").await;
    assert_eq!(body, json!({"answers": [["No"]]}));
}

#[test]
fn v2_tools_are_scoped_and_retired_and_real_failures_are_visible() {
    let mut tools = HashMap::new();
    let event = |kind, session, message, name| {
        json!({"type":kind,"data":{
            "sessionID":session,"assistantMessageID":message,"id":"same","name":name,
            "error":{"type":"permission.denied","message":"Denied"}
        }})
    };
    let calls = [
        ("a", "m1", "read"),
        ("b", "m1", "bash"),
        ("a", "m2", "write"),
    ];
    for (session, message, name) in calls {
        normalize_v2_frame(
            event("session.tool.input.started", session, message, name),
            &mut tools,
        );
    }
    for (session, message, name) in calls {
        let out = normalize_v2_frame(
            event("session.tool.failed", session, message, ""),
            &mut tools,
        );
        assert_eq!(out[0].pointer("/properties/part/tool").unwrap(), name);
        assert_eq!(
            out[0].pointer("/properties/part/state/status").unwrap(),
            "error"
        );
        assert_eq!(
            out[0].pointer("/properties/part/callID").unwrap(),
            &json!(format!("{session}:{message}:same"))
        );
    }
    assert!(tools.is_empty());
    normalize_v2_frame(
        event("session.tool.input.started", "a", "m1", "read"),
        &mut tools,
    );
    normalize_v2_frame(
        event("session.execution.interrupted", "a", "m1", ""),
        &mut tools,
    );
    assert!(tools.is_empty());
}

#[tokio::test]
async fn v2_reasoning_deltas_and_failed_tools_reach_the_feed() {
    let mut wire = TurnWire::start_proto(false, true).await;
    wire.request("/api/model").await;
    wire.request("/prompt").await;
    wire.v2("session.execution.started", json!({"sessionID":"fixture"}));
    wire.v2(
        "session.step.started",
        json!({"sessionID":"fixture","assistantMessageID":"m"}),
    );
    wire.v2(
        "session.reasoning.started",
        json!({"sessionID":"fixture","assistantMessageID":"m","ordinal":0}),
    );
    wire.v2(
        "session.reasoning.delta",
        json!({"sessionID":"fixture","assistantMessageID":"m","ordinal":0,"delta":"Thinking"}),
    );
    wire.v2(
        "session.tool.input.started",
        json!({"sessionID":"fixture","assistantMessageID":"m","id":"c","name":"read"}),
    );
    wire.v2(
        "session.tool.failed",
        json!({"sessionID":"fixture","assistantMessageID":"m","id":"c","error":{"message":"Denied"}}),
    );
    wire.v2(
        "session.execution.succeeded",
        json!({"sessionID":"fixture"}),
    );
    let mut reasoning = String::new();
    let mut failed = false;
    loop {
        match wire.next_event().await {
            AgentEvent::ReasoningDelta { text } => reasoning.push_str(&text),
            AgentEvent::ToolResult { is_error, .. } => failed |= is_error,
            AgentEvent::Done { .. } => break,
            _ => {}
        }
    }
    assert_eq!(reasoning, "Thinking");
    assert!(failed);
}

#[tokio::test]
async fn prompt_error_and_interrupt_before_busy_still_settle() {
    let mut wire = TurnWire::start(false).await;
    wire.request("/prompt_async").await;
    wire.bus.send(json!({"type":"session.error", "properties":{"sessionID":"fixture", "error":{"name":"ProviderError", "data":{"message":"bad model"}}}})).unwrap();
    wire.idle();
    assert_eq!(wire.done().await.0, DoneStatus::Errored);

    let mut wire = TurnWire::start(false).await;
    wire.request("/prompt_async").await;
    wire.interrupt.cancel();
    wire.request("/abort").await;
    wire.idle();
    assert_eq!(wire.done().await.0, DoneStatus::Interrupted);
}

#[tokio::test]
async fn v2_model_selection_and_prompt_use_the_documented_bodies() {
    let mut wire = TurnWire::start_proto(false, true).await;
    wire.request("/api/model").await;
    wire.request("/prompt").await;
    let posts = wire.posts.lock().unwrap();
    assert_eq!(
        posts
            .iter()
            .find(|(path, _)| path == "/api/session/fixture/model")
            .unwrap()
            .1,
        json!({"model":{"providerID":"opencode","id":"muse","variant":"low"}})
    );
    assert_eq!(
        posts
            .iter()
            .find(|(path, _)| path == "/api/session/fixture/prompt")
            .unwrap()
            .1,
        json!({"text":"first","files":[]})
    );
}

#[tokio::test]
async fn v2_pending_tool_overflow_fails_the_run_instead_of_growing_forever() {
    let mut wire = TurnWire::start_proto(false, true).await;
    wire.request("/api/model").await;
    wire.request("/prompt").await;
    for index in 0..=MAX_PENDING_V2_TOOLS {
        wire.v2(
            "session.tool.input.started",
            json!({"sessionID":"other","assistantMessageID":"m","id":format!("c{index}"),"name":"read"}),
        );
    }
    assert_eq!(wire.done().await.0, DoneStatus::Errored);
}

#[tokio::test]
async fn v2_session_model_overflow_drops_the_cache_instead_of_failing_the_run() {
    let mut wire = TurnWire::start_proto(false, true).await;
    wire.request("/api/model").await;
    wire.request("/prompt").await;
    wire.v2("session.execution.started", json!({"sessionID": "fixture"}));
    for index in 0..=MAX_V2_SESSION_MODELS {
        wire.v2(
            "session.step.started",
            json!({
                "sessionID": format!("other-{index}"),
                "assistantMessageID": "m",
                "model": {"id": "muse", "providerID": "opencode"},
            }),
        );
    }
    wire.v2(
        "session.execution.succeeded",
        json!({"sessionID": "fixture"}),
    );
    assert_eq!(wire.done().await.0, DoneStatus::Completed);
}

#[tokio::test]
async fn v2_external_interrupt_is_not_reported_as_success() {
    let mut wire = TurnWire::start_proto(false, true).await;
    wire.request("/api/model").await;
    wire.request("/prompt").await;
    wire.v2(
        "session.execution.interrupted",
        json!({"sessionID":"fixture","reason":"shutdown"}),
    );
    assert_eq!(wire.done().await.0, DoneStatus::Interrupted);
}

#[tokio::test]
async fn v2_recovered_step_failure_does_not_poison_successful_execution() {
    let mut wire = TurnWire::start_proto(false, true).await;
    wire.request("/api/model").await;
    wire.request("/prompt").await;
    wire.v2("session.execution.started", json!({"sessionID":"fixture"}));
    wire.v2(
        "session.step.failed",
        json!({"sessionID":"fixture","error":{"type":"provider.rate_limit","message":"Retrying"}}),
    );
    wire.v2(
        "session.step.started",
        json!({"sessionID":"fixture","assistantMessageID":"recovered"}),
    );
    wire.v2(
        "session.text.ended",
        json!({"sessionID":"fixture","assistantMessageID":"recovered","ordinal":0,"text":"Recovered"}),
    );
    wire.v2(
        "session.execution.succeeded",
        json!({"sessionID":"fixture"}),
    );
    let (status, text) = wire.done().await;
    assert_eq!(status, DoneStatus::Completed);
    assert_eq!(text, "Recovered");
}

#[test]
fn server_version_parsing() {
    for (raw, expected) in [
        ("2.0.4", Some((2, 0, 4))),
        ("opencode v2.0.11", Some((2, 0, 11))),
        ("v2.0.11-beta+build", Some((2, 0, 11))),
        (" 1.18.21 ", Some((1, 18, 21))),
        ("3.1.0", Some((3, 1, 0))),
        ("2.0", None),
        ("", None),
        ("unknown", None),
    ] {
        let version = ServerVersion::parse(raw);
        assert_eq!(version.raw, raw);
        assert_eq!(version.number, expected, "{raw}");
    }
}

#[tokio::test]
async fn detection_routes_and_authentication() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    for (route, body, status, expected) in [
        (
            "/api/info",
            r#"{"version":"2.0.11"}"#,
            200,
            Some(Protocol::V2),
        ),
        (
            "/api/status",
            r#"{"data":{"version":"2.0.4"}}"#,
            201,
            Some(Protocol::V2),
        ),
        (
            "/api/health",
            r#"{"healthy":true,"version":"2.0.3"}"#,
            200,
            Some(Protocol::V2),
        ),
        (
            "/global/health",
            r#"{"version":"1.18.21"}"#,
            200,
            Some(Protocol::V1),
        ),
        ("/api/health", r#"{"healthy":true}"#, 200, None),
        ("/api/info", r#"{"version":" "}"#, 200, None),
        ("/api/info", "<html>web UI</html>", 200, None),
        ("/api/info", "{}", 401, None),
        ("/api/status", "{}", 403, None),
    ] {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let server =
            Server::attached(format!("http://{}", listener.local_addr().unwrap())).unwrap();
        let seen = Arc::new(std::sync::Mutex::new(Vec::new()));
        let recorded = seen.clone();
        let task = tokio::spawn(async move {
            loop {
                let (mut socket, _) = listener.accept().await.unwrap();
                let mut bytes = [0; 4096];
                let read = socket.read(&mut bytes).await.unwrap();
                let header = String::from_utf8_lossy(&bytes[..read]);
                let path = header.split_whitespace().nth(1).unwrap();
                recorded.lock().unwrap().push(path.to_owned());
                let (code, text) = if path == route {
                    (status, body)
                } else {
                    (404, "{}")
                };
                socket.write_all(format!("HTTP/1.1 {code} Response\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{text}", text.len()).as_bytes()).await.unwrap();
            }
        });
        let result = server.detect().await;
        task.abort();
        if status == 401 || status == 403 {
            assert!(
                result
                    .unwrap_err()
                    .to_string()
                    .contains("authentication rejected")
            );
        } else {
            assert_eq!(result.unwrap(), expected);
            assert_eq!(server.version.get().is_some(), expected.is_some());
        }
        let seen = seen.lock().unwrap();
        let order = ["/api/info", "/api/status", "/api/health", "/global/health"];
        assert_eq!(*seen, order[..seen.len()]);
    }
}

#[test]
fn v2_command_bodies_follow_server_version() {
    for version in ["2.0.2", "2.0.3", "unknown"] {
        assert_eq!(
            command_body_v2(Some(&ServerVersion::parse(version)), "test", "args", &[]),
            json!({"command":"test","text":"args"})
        );
    }
    for version in ["2.0.4", "v2.0.11", "3.0.0"] {
        let version = ServerVersion::parse(version);
        assert_eq!(
            command_body_v2(Some(&version), "test", "args", &[]),
            json!({"name":"test","text":"args"})
        );
        let attachments = vec!["/workspace/image.png".into()];
        assert_eq!(
            command_body_v2(Some(&version), "test", "args", &attachments)["files"],
            prompt_body_v2("args", &attachments)["files"]
        );
    }
}

#[tokio::test]
async fn command_http_failure_errors_turn_without_watchdog() {
    for (v2, version, key) in [
        (false, "1.18.21", "command"),
        (true, "2.0.3", "command"),
        (true, "2.0.11", "name"),
    ] {
        let mut wire = TurnWire::start_fixture(Fixture {
            v2,
            version,
            overrides: json!({"prompt": "/test args"}),
            command_failure: true,
            ..Fixture::default()
        })
        .await;
        let error = tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                if let AgentEvent::Done { status, error, .. } =
                    wire.events.recv().await.unwrap().unwrap()
                {
                    assert_eq!(status, DoneStatus::Errored);
                    return error.unwrap();
                }
            }
        })
        .await
        .unwrap();
        assert!(error.contains("400"), "{error}");
        assert!(error.contains("bad command!"), "{error}");
        let posts = wire.posts.lock().unwrap();
        let (_, body) = posts
            .iter()
            .find(|(path, _)| path.ends_with("/command"))
            .unwrap();
        assert_eq!(body[key], "test");
        assert_eq!(body[if v2 { "text" } else { "arguments" }], "args");
    }
}

#[test]
fn agent_model_option_filters_and_preserves_ids() {
    let option = agent_option(&json!({"data":[
        {"id":"build-id","name":"Build","mode":"primary","hidden":false},
        {"id":"all-id","name":"All","mode":"all"},
        {"id":"hidden","name":"Hidden","mode":"primary","hidden":true},
        {"id":"sub","name":"Sub","mode":"subagent"}
    ]}));
    assert_eq!(option.id, "agent");
    assert_eq!(option.label, "Agent");
    assert_eq!(option.default_choice, "");
    assert_eq!(
        option
            .choices
            .iter()
            .map(|choice| (choice.id.as_str(), choice.label.as_str()))
            .collect::<Vec<_>>(),
        vec![
            ("", "Server default"),
            ("build-id", "Build"),
            ("all-id", "All")
        ]
    );
}

#[tokio::test]
async fn agent_selection_on_create_and_resume() {
    for v2 in [false, true] {
        for resume in [false, true] {
            for agent in [json!("build-id"), json!(""), json!(true)] {
                let mut overrides = json!({"modelOptions": {"agent": agent}});
                if resume {
                    overrides["resume"] = json!("fixture");
                }
                let mut wire = TurnWire::start_config(v2, "2.0.11", overrides).await;
                if v2 {
                    wire.request("/api/model").await;
                }
                wire.request(if v2 { "/prompt" } else { "/prompt_async" })
                    .await;
                let posts = wire.posts.lock().unwrap();
                let selection = posts.iter().find(|(path, _)| {
                    if resume {
                        path.ends_with("/agent")
                    } else {
                        path.ends_with("/session")
                    }
                });
                let expected = if v2 && agent == "build-id" {
                    json!("build-id")
                } else {
                    Value::Null
                };
                assert_eq!(
                    selection
                        .map(|(_, body)| body["agent"].clone())
                        .unwrap_or(Value::Null),
                    expected
                );
                if resume && v2 && agent == "build-id" {
                    assert_eq!(
                        posts[0],
                        (
                            "/api/session/fixture/agent".into(),
                            json!({"agent":"build-id"})
                        )
                    );
                }
            }
        }
    }
}

#[test]
fn v2_status_retry_and_progress_vocabulary() {
    let mut names = HashMap::new();
    for status in [
        json!({"type":"busy"}),
        json!({"type":"idle"}),
        json!({"type":"retry","attempt":3,"message":"overloaded","next":123}),
    ] {
        let data = json!({"sessionID":"s", "status":status});
        assert_eq!(
            normalize_v2_frame(json!({"type":"session.status", "data":data}), &mut names),
            vec![json!({"type":"session.status","properties":data})]
        );
    }
    let retry = normalize_v2_frame(
        json!({"type":"session.retry.scheduled","data":{"sessionID":"s","assistantMessageID":"m","attempt":3,"at":123,"error":{"type":"provider.api","message":"overloaded"}}}),
        &mut names,
    );
    assert_eq!(
        retry[0],
        json!({"type":"session.status","properties":{"sessionID":"s","status":{"type":"retry","attempt":3,"next":123,"message":"overloaded"}}})
    );
    normalize_v2_frame(
        json!({"type":"session.tool.input.started","data":{"sessionID":"s","assistantMessageID":"m","id":"tool","name":"task"}}),
        &mut names,
    );
    let progress = normalize_v2_frame(
        json!({"type":"session.tool.progress","data":{"sessionID":"s","assistantMessageID":"m","id":"tool","metadata":{"sessionId":"child"}}}),
        &mut names,
    );
    assert_eq!(
        progress[0]["properties"]["part"]["state"],
        json!({"status":"running","metadata":{"sessionId":"child"}})
    );
    assert_eq!(progress[0]["properties"]["part"]["tool"], "task");
    assert!(
        normalize_v2_frame(
            json!({"type":"session.future.event","data":{"sessionID":"s"}}),
            &mut names
        )
        .is_empty()
    );
}

#[tokio::test]
async fn v2_discovery_settles_and_caches_agents_with_overlapping_models() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let harness =
        OpencodeHarness::new().with_base_url(format!("http://{}", listener.local_addr().unwrap()));
    let calls = Arc::new(std::sync::Mutex::new(Vec::new()));
    let recorded = calls.clone();
    let task = tokio::spawn(async move {
        loop {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut bytes = [0; 4096];
            let read = socket.read(&mut bytes).await.unwrap();
            let request = String::from_utf8_lossy(&bytes[..read]);
            let path = request.split_whitespace().nth(1).unwrap();
            let body = {
                let mut calls = recorded.lock().unwrap();
                calls.push(path.to_owned());
                match path {
                    "/api/info" => json!({"version":"2.0.11"}),
                    "/api/model" if calls.iter().filter(|p| p.as_str() == path).count() == 1 => {
                        json!({"data":[]})
                    }
                    "/api/model" => json!({"data":[
                        {"providerID":"mock","id":"a","name":"A","enabled":true},
                        {"providerID":"mock","id":"b","name":"B","enabled":true}
                    ]}),
                    "/api/agent" => json!({"data":[{"id":"agent-id","name":"Agent name","mode":"all","hidden":false}]}),
                    _ => json!({"data":[]}),
                }
            }
            .to_string();
            socket
                .write_all(
                    format!(
                        "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                        body.len()
                    )
                    .as_bytes(),
                )
                .await
                .unwrap();
        }
    });
    let count = |path: &str| {
        calls
            .lock()
            .unwrap()
            .iter()
            .filter(|seen| seen.as_str() == path)
            .count()
    };
    let (first, overlapping) = tokio::join!(harness.models(), harness.models());
    let first = first.unwrap();
    assert_eq!(first.len(), 2);
    assert_eq!(first, overlapping.unwrap());
    for model in &first {
        assert_eq!(model.options.len(), 1);
        assert_eq!(model.options[0].id, "agent");
        assert_eq!(model.options[0].choices[1].id, "agent-id");
    }
    assert_eq!(count("/api/model"), 2, "empty catalog must settle");
    assert_eq!(
        count("/api/agent"),
        1,
        "overlapping callers share agents with models"
    );
    harness.model_catalog(true).await.unwrap();
    assert_eq!(
        count("/api/agent"),
        2,
        "later discovery refreshes agents too"
    );
    task.abort();
    task.await.ok();
    let retained = harness.model_catalog(true).await.unwrap();
    assert_eq!(retained.source, "cache");
    assert_eq!(
        retained.models, first,
        "offline refresh retains models and agent options"
    );
}

#[tokio::test]
async fn v2_scheduled_retries_reach_existing_retry_abort() {
    let mut wire = TurnWire::start_proto(false, true).await;
    wire.request("/api/model").await;
    wire.request("/prompt").await;
    wire.v2(
        "session.retry.scheduled",
        json!({"sessionID":"fixture","assistantMessageID":"m","attempt":RETRY_ABORT_ATTEMPT,"at":123,"error":{"type":"provider.api","message":"overloaded"}}),
    );
    wire.request("/interrupt").await;
    // 2.x answers our own abort with an interrupt, which must still read as the retry failure.
    wire.v2(
        "session.execution.interrupted",
        json!({"sessionID":"fixture","reason":"user"}),
    );
    let error = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if let AgentEvent::Done { status, error, .. } = wire.next_event().await {
                assert_eq!(status, DoneStatus::Errored);
                return error;
            }
        }
    })
    .await
    .unwrap()
    .unwrap();
    assert!(error.contains("kept failing"), "{error}");
    assert!(error.contains("overloaded"), "{error}");
}
#[tokio::test]
async fn stalled_prompt_post_does_not_block_bus_completion() {
    let mut wire = TurnWire::start_config(false, "1.18.31", json!({"holdPrompt": true})).await;
    wire.request("/prompt_async").await;
    wire.status("busy");
    wire.status("idle");
    wire.idle();
    assert_eq!(wire.done().await.0, DoneStatus::Completed);
    assert_eq!(wire.polls.load(std::sync::atomic::Ordering::SeqCst), 0);
}

#[tokio::test]
async fn stalled_prompt_post_has_a_bounded_timeout() {
    let mut wire = TurnWire::start_config(false, "1.18.31", json!({"holdPrompt": true})).await;
    wire.request("/prompt_async").await;
    wire.status("busy");
    tokio::task::yield_now().await;
    tokio::time::pause();
    tokio::time::advance(server::CALL_TIMEOUT + Duration::from_secs(1)).await;
    assert_eq!(wire.done().await.0, DoneStatus::Errored);
}

#[tokio::test]
async fn ambiguous_idle_polls_status_with_backoff_until_idle() {
    let mut wire = TurnWire::start_config(false, "1.18.31", json!({"busyPolls": 2})).await;
    wire.request("/prompt_async").await;
    wire.status("busy");
    wire.idle();
    let start = tokio::time::Instant::now();
    assert_eq!(wire.done().await.0, DoneStatus::Completed);
    assert_eq!(wire.polls.load(std::sync::atomic::Ordering::SeqCst), 3);
    assert!(start.elapsed() >= Duration::from_millis(650));
}

#[tokio::test]
async fn abort_ignores_late_bus_text_and_usage() {
    let mut wire = TurnWire::start(false).await;
    wire.request("/prompt_async").await;
    wire.status("busy");
    wire.interrupt.cancel();
    wire.request("/abort").await;
    wire.bus.send(json!({"type":"message.updated", "properties":{"info":{"id":"late", "sessionID":"fixture", "role":"assistant", "tokens":{"input":999,"output":999}}}})).unwrap();
    wire.bus.send(json!({"type":"message.part.updated", "properties":{"part":{"id":"text", "messageID":"late", "sessionID":"fixture", "type":"text", "text":"LATE"}}})).unwrap();
    wire.idle();
    let mut dones = 0;
    while let Some(event) = wire.events.recv().await {
        match event.unwrap() {
            AgentEvent::TextDelta { .. } | AgentEvent::Usage { .. } => {
                panic!("late content after abort")
            }
            AgentEvent::Done { status, .. } => {
                assert_eq!(status, DoneStatus::Interrupted);
                dones += 1;
            }
            _ => {}
        }
    }
    assert_eq!(dones, 1);
}

#[tokio::test]
async fn idle_without_busy_resolves_through_status_poll() {
    let mut wire = TurnWire::start(false).await;
    wire.request("/prompt_async").await;
    wire.idle();
    assert_eq!(wire.done().await.0, DoneStatus::Completed);
    assert_eq!(wire.polls.load(std::sync::atomic::Ordering::SeqCst), 1);
}

#[tokio::test]
async fn v2_spawn_names_bind_child_traffic_to_the_parent_chip() {
    for name in ["task", "subagent"] {
        let mut wire = TurnWire::start_proto(false, true).await;
        wire.request("/api/model").await;
        wire.request("/prompt").await;
        wire.v2("session.execution.started", json!({"sessionID":"fixture"}));
        wire.v2(
            "session.step.started",
            json!({"sessionID":"fixture","assistantMessageID":"parent-message"}),
        );
        wire.v2(
            "session.tool.input.started",
            json!({"sessionID":"fixture","assistantMessageID":"parent-message","id":"spawn","name":name}),
        );
        wire.v2(
            "session.tool.called",
            json!({"sessionID":"fixture","assistantMessageID":"parent-message","id":"spawn","input":{"description":"Inspect project","prompt":"inspect"}}),
        );
        wire.v2(
            "session.created",
            json!({"sessionID":"child","parentID":"fixture","title":"Inspect project"}),
        );
        wire.v2(
            "session.step.started",
            json!({"sessionID":"child","assistantMessageID":"child-message"}),
        );
        wire.v2(
            "session.text.delta",
            json!({"sessionID":"child","assistantMessageID":"child-message","ordinal":0,"delta":"child answer"}),
        );
        wire.v2("session.execution.succeeded", json!({"sessionID":"child"}));
        wire.v2(
            "session.tool.success",
            json!({"sessionID":"fixture","assistantMessageID":"parent-message","id":"spawn","content":[]}),
        );
        wire.v2(
            "session.execution.succeeded",
            json!({"sessionID":"fixture"}),
        );
        let mut calls = Vec::new();
        let mut child_events = Vec::new();
        tokio::time::timeout(Duration::from_secs(5), async {
            while let Some(event) = wire.events.recv().await {
                match event.unwrap() {
                    AgentEvent::ToolCall { id, call } => calls.push((id, call)),
                    AgentEvent::Subagent {
                        parent_tool_use_id,
                        event,
                    } => child_events.push((parent_tool_use_id, event)),
                    AgentEvent::Done { status, .. } => {
                        assert_eq!(status, DoneStatus::Completed);
                        break;
                    }
                    _ => {}
                }
            }
        })
        .await
        .unwrap();
        assert!(
            matches!(&calls[0].1, ToolCall::Unknown { name, .. } if name == "Agent: Inspect project")
        );
        assert!(
            child_events.iter().any(|(id, event)| id == &calls[0].0
                && matches!(event.as_ref(), AgentEvent::TextDelta { text } if text == "child answer")),
            "{name}: {child_events:?}"
        );
    }
}

#[cfg(unix)]
fn fake_opencode(directory: &Path) -> PathBuf {
    use std::os::unix::fs::PermissionsExt;
    let executable = directory.join("opencode");
    std::fs::write(
        &executable,
        r#"#!/usr/bin/env python3
import json, os, sys
from http.server import BaseHTTPRequestHandler, HTTPServer
port = int(sys.argv[sys.argv.index("--port") + 1])
class Handler(BaseHTTPRequestHandler):
    def do_GET(self):
        if self.path == "/global/health":
            body = {"healthy": True, "version": "1.18.20"}
        elif self.path == "/config-probe":
            body = {
                "config": os.environ.get("OPENCODE_CONFIG_CONTENT"),
                "client": os.environ.get("OPENCODE_CLIENT"),
                "username": os.environ.get("OPENCODE_SERVER_USERNAME"),
                "password": os.environ.get("OPENCODE_PASSWORD"),
                "legacy": os.environ.get("OPENCODE_SERVER_PASSWORD"),
                "authorization": self.headers.get("Authorization"),
                "cwd": os.getcwd(),
            }
        else:
            self.send_response(404)
            self.end_headers()
            return
        data = json.dumps(body).encode()
        self.send_response(200)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(data)))
        self.end_headers()
        self.wfile.write(data)
    def log_message(self, *args):
        pass
HTTPServer(("127.0.0.1", port), Handler).serve_forever()
"#,
    )
    .unwrap();
    std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o700)).unwrap();
    executable
}

#[cfg(unix)]
#[tokio::test]
async fn spawned_server_gets_password_username_and_client() {
    use base64::Engine as _;
    let fixture = tempfile::tempdir().unwrap();
    let project = tempfile::tempdir().unwrap();
    let harness = OpencodeHarness::new()
        .with_executable(fake_opencode(fixture.path()))
        .with_graces(Duration::from_millis(100), Duration::from_millis(100));
    let mut server = harness
        .boot(project.path().to_str())
        .unwrap()
        .await
        .unwrap();
    assert_eq!(server.protocol().await, Protocol::V1);
    let probe = server.get_json("/config-probe", None).await.unwrap();
    server.shutdown(Duration::from_millis(100)).await;
    assert_eq!(
        probe["config"].as_str(),
        std::env::var("OPENCODE_CONFIG_CONTENT").ok().as_deref(),
        "inherited config content passes through untouched"
    );
    assert_eq!(probe["client"], "wu");
    assert_eq!(probe["username"], "opencode");
    let password = probe["password"].as_str().unwrap();
    assert_eq!(probe["legacy"], password);
    assert_eq!(
        probe["authorization"],
        format!(
            "Basic {}",
            base64::engine::general_purpose::STANDARD.encode(format!("opencode:{password}"))
        )
    );
    assert_eq!(
        Path::new(probe["cwd"].as_str().unwrap())
            .canonicalize()
            .unwrap(),
        project.path().canonicalize().unwrap()
    );
}
#[cfg(unix)]
#[tokio::test]
async fn crashed_server_reports_its_stderr() {
    use std::os::unix::fs::PermissionsExt;
    let fixture = tempfile::tempdir().unwrap();
    let executable = fixture.path().join("opencode");
    std::fs::write(
        &executable,
        "#!/bin/sh\necho 'config is broken' >&2\nexit 3\n",
    )
    .unwrap();
    std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o700)).unwrap();
    let harness = OpencodeHarness::new().with_executable(executable);
    let error = harness.boot(None).unwrap().await.err().unwrap().to_string();
    assert!(error.contains("exit code 3"), "{error}");
    assert!(error.contains("config is broken"), "{error}");
}

#[cfg(unix)]
#[tokio::test]
async fn stop_during_boot_ends_the_run_and_kills_the_server() {
    use std::os::unix::fs::PermissionsExt;
    let fixture = tempfile::tempdir().unwrap();
    let executable = fixture.path().join("opencode");
    let pid_file = fixture.path().join("pid");
    std::fs::write(
        &executable,
        format!(
            "#!/bin/sh\necho $$ > '{}'\nexec sleep 600\n",
            pid_file.display()
        ),
    )
    .unwrap();
    std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o700)).unwrap();
    let harness = OpencodeHarness::new().with_executable(executable);
    let (_steer, steering) = mpsc::channel(4);
    let interrupt = tokio_util::sync::CancellationToken::new();
    let request: RunRequest =
        serde_json::from_value(json!({"prompt": "hi", "cwd": "", "permission": "ask"})).unwrap();
    let mut stream = tokio::time::timeout(
        Duration::from_secs(2),
        harness.run(
            request,
            RunControls {
                request_input: Box::new(|_| tokio::sync::oneshot::channel().1),
                steering,
                interrupt: interrupt.clone(),
            },
        ),
    )
    .await
    .expect("run returns before opencode is ready")
    .unwrap();
    let pid: i32 = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if let Some(pid) = std::fs::read_to_string(&pid_file)
                .ok()
                .and_then(|pid| pid.trim().parse().ok())
            {
                return pid;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
    interrupt.cancel();
    let done = tokio::time::timeout(Duration::from_secs(2), stream.next())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert!(
        matches!(
            done,
            AgentEvent::Done {
                status: DoneStatus::Interrupted,
                ..
            }
        ),
        "{done:?}"
    );
    tokio::time::timeout(Duration::from_secs(5), async {
        // SAFETY: signal 0 only checks that the process still exists.
        while unsafe { libc::kill(pid, 0) } == 0 {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("opencode serve was not killed");
}

#[tokio::test]
async fn a_missing_project_folder_is_reported_as_such() {
    let harness = OpencodeHarness::new().with_base_url("http://127.0.0.1:9");
    let (_steer, steering) = mpsc::channel(4);
    let request: RunRequest = serde_json::from_value(
        json!({"prompt": "hi", "cwd": "/definitely/not/a/wu/folder", "permission": "auto"}),
    )
    .unwrap();
    let error = harness
        .run(
            request,
            RunControls {
                request_input: Box::new(|_| tokio::sync::oneshot::channel().1),
                steering,
                interrupt: tokio_util::sync::CancellationToken::new(),
            },
        )
        .await
        .err()
        .unwrap()
        .to_string();
    assert!(
        error.contains("project folder not found: /definitely/not/a/wu/folder"),
        "{error}"
    );
}

#[tokio::test]
async fn a_late_prompt_failure_does_not_end_a_settled_turn() {
    let mut wire = TurnWire::start_config(
        false,
        "1.18.31",
        json!({"failPromptDelayMs": 300, "keepSteering": true}),
    )
    .await;
    wire.request("/prompt_async").await;
    wire.status("busy");
    wire.status("idle");
    wire.idle();
    assert_eq!(wire.done().await.0, DoneStatus::Completed);
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert!(!wire.run.is_finished(), "a stale failure ended the run");
    wire.steering
        .as_ref()
        .unwrap()
        .send(steer("next"))
        .await
        .unwrap();
    wire.request("/prompt_async").await;
    wire.status("busy");
    wire.status("idle");
    wire.idle();
    assert_eq!(wire.done().await.0, DoneStatus::Completed);
}

#[tokio::test]
async fn a_failed_permission_reply_ends_the_turn_with_an_error() {
    for permission in ["full_access", "ask"] {
        let mut wire = TurnWire::start_fixture(Fixture {
            permission,
            answer: Some(true),
            overrides: json!({"failPermissionReply": true}),
            ..Fixture::default()
        })
        .await;
        wire.request("/prompt_async").await;
        wire.status("busy");
        wire.bus
            .send(json!({"type": "permission.asked", "properties": {
                "id": "per_1", "sessionID": "fixture", "permission": "bash", "patterns": ["make"]
            }}))
            .unwrap();
        let (status, error, surfaced) = tokio::time::timeout(Duration::from_secs(5), async {
            let mut surfaced = None;
            loop {
                match wire.next_event().await {
                    AgentEvent::Error { message } => surfaced = Some(message),
                    AgentEvent::Done { status, error, .. } => return (status, error, surfaced),
                    _ => {}
                }
            }
        })
        .await
        .unwrap();
        assert_eq!(status, DoneStatus::Errored, "{permission}");
        assert!(
            error
                .unwrap()
                .contains("Couldn't answer OpenCode's permission request")
        );
        assert!(surfaced.is_some());
    }
}

#[tokio::test]
async fn an_unreachable_event_stream_fails_before_prompting() {
    let mut wire = TurnWire::start_config(false, "1.18.31", json!({"noBus": true})).await;
    let error = tokio::time::timeout(Duration::from_secs(20), async {
        loop {
            if let AgentEvent::Done { status, error, .. } =
                wire.next_event_within(Duration::from_secs(20)).await
            {
                assert_eq!(status, DoneStatus::Errored);
                return error.unwrap();
            }
        }
    })
    .await
    .unwrap();
    assert!(
        error.contains("Couldn't connect to opencode's event stream"),
        "{error}"
    );
    assert!(
        !wire
            .posts
            .lock()
            .unwrap()
            .iter()
            .any(|(path, _)| path.ends_with("/prompt_async"))
    );
}

#[tokio::test]
async fn resume_retries_once_and_only_starts_fresh_when_missing() {
    for (resume, failures, expected) in [
        ("flaky", 1, Some("flaky")),
        ("missing", 0, Some("fixture")),
        ("flaky", 2, None),
    ] {
        let mut wire = TurnWire::start_config(
            false,
            "1.18.31",
            json!({"resume": resume, "resumeFailures": failures}),
        )
        .await;
        let event = wire.next_event().await;
        match expected {
            Some(expected) => assert!(
                matches!(&event, AgentEvent::SessionStarted { session_id, .. } if session_id == expected),
                "{resume}/{failures}: {event:?}"
            ),
            None => {
                let mut event = event;
                while !matches!(event, AgentEvent::Done { .. }) {
                    event = wire.next_event().await;
                }
                assert!(
                    matches!(&event, AgentEvent::Done { status: DoneStatus::Errored, error: Some(error), .. } if error.contains("500")),
                    "{event:?}"
                );
            }
        }
        let created = wire
            .posts
            .lock()
            .unwrap()
            .iter()
            .any(|(path, _)| path == "/session");
        assert_eq!(created, resume == "missing", "{resume}/{failures}");
    }
}

#[tokio::test]
async fn an_ending_run_closes_steering_before_done() {
    let mut wire = TurnWire::start_config(false, "1.18.31", json!({"keepSteering": true})).await;
    wire.request("/prompt_async").await;
    wire.bus.send(json!({"type":"session.error", "properties":{"sessionID":"fixture", "error":{"name":"ProviderError", "data":{"message":"bad model"}}}})).unwrap();
    wire.idle();
    assert_eq!(wire.done().await.0, DoneStatus::Errored);
    assert!(
        wire.steering
            .as_ref()
            .unwrap()
            .try_send(steer("lost?"))
            .is_err(),
        "a message sent after the final Done must be refused, not swallowed"
    );
}

#[tokio::test]
async fn ask_mode_sends_session_rules_and_auto_sends_none() {
    let agents = json!([{"name": "build", "mode": "primary", "permission": [
        {"permission": "*", "pattern": "*", "action": "allow"},
        {"permission": "bash", "pattern": "rm *", "action": "deny"},
    ]}]);
    for (permission, expect_rules) in [
        ("ask", true),
        ("accept_edits", true),
        ("auto", false),
        ("full_access", false),
    ] {
        let mut wire = TurnWire::start_fixture(Fixture {
            permission,
            overrides: json!({"agents": agents}),
            ..Fixture::default()
        })
        .await;
        wire.request("/prompt_async").await;
        let body = wire.posted("/session").await;
        assert_eq!(
            body.get("permission").is_some(),
            expect_rules,
            "{permission}"
        );
        if expect_rules {
            let rules = body["permission"].as_array().unwrap();
            assert!(
                rules.contains(&json!({"permission": "bash", "pattern": "rm *", "action": "deny"}))
            );
            assert!(
                rules.contains(&json!({"permission": "bash", "pattern": "*", "action": "ask"}))
            );
            assert!(
                rules
                    .iter()
                    .all(|rule| rule["action"] != "allow" || rule["permission"] == "edit")
            );
        }
    }
}

#[tokio::test]
async fn resuming_restates_rules_with_a_patch() {
    let agents = json!([{"name": "build", "mode": "primary", "permission": [
        {"permission": "*", "pattern": "*", "action": "allow"},
    ]}]);
    let mut wire = TurnWire::start_fixture(Fixture {
        permission: "ask",
        overrides: json!({"agents": agents, "resume": "fixture"}),
        ..Fixture::default()
    })
    .await;
    wire.request("/prompt_async").await;
    let body = wire.posted("/session/fixture").await;
    assert_eq!(
        body["permission"][0],
        json!({"permission": "edit", "pattern": "*", "action": "ask"})
    );
    assert!(
        !wire
            .posts
            .lock()
            .unwrap()
            .iter()
            .any(|(path, _)| path == "/session"),
        "resume must not fork"
    );
}

#[tokio::test]
async fn unreadable_rules_warn_and_fall_back_to_opencodes_own() {
    let mut wire = TurnWire::start_fixture(Fixture {
        permission: "ask",
        ..Fixture::default()
    })
    .await;
    assert!(matches!(
        wire.next_event().await,
        AgentEvent::SessionStarted { .. }
    ));
    assert!(matches!(
        wire.next_event().await,
        AgentEvent::Error { message } if message.contains("Couldn't read OpenCode's permission rules")
    ));
    assert!(wire.posted("/session").await.get("permission").is_none());
}
