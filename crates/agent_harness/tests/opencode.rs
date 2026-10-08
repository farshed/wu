use std::sync::{Arc, Mutex};
use std::time::Duration;

use futures::StreamExt;
use serde_json::{Value, json};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::sync::{broadcast, mpsc, oneshot};

use agent_harness::claude::{PERMISSION_ALLOW, PERMISSION_DENY, PERMISSION_HEADER};
use agent_harness::{
    AgentEvent, DoneStatus, PermissionMode, ReasoningLevel, RunRequest, SkillRef, ToolCall,
    UserInputAnswer,
};
use agent_harness::{
    CancellationToken, Harness, HarnessError, OpencodeHarness, RunControls, SteerMessage,
};

#[derive(Clone)]
struct FakeOpencode {
    base: String,
    events: broadcast::Sender<(u64, String)>,
    backlog: Arc<Mutex<Vec<(u64, String)>>>,
    posts: Arc<Mutex<Vec<(String, Value)>>>,
    providers: Arc<Mutex<Value>>,
    statuses: Arc<Mutex<serde_json::Map<String, Value>>>,
    commands: Arc<Mutex<Value>>,
    first_prompt_had_subscriber: Arc<Mutex<Option<bool>>>,
    fail_session_creates: Arc<Mutex<u32>>,
    config: Arc<Mutex<Value>>,
}

impl FakeOpencode {
    async fn start() -> Self {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let (events, _) = broadcast::channel::<(u64, String)>(256);
        let fake = Self {
            base,
            events,
            backlog: Arc::default(),
            posts: Arc::default(),
            providers: Arc::new(Mutex::new(json!({ "all": [], "default": {} }))),
            statuses: Arc::default(),
            commands: Arc::new(Mutex::new(
                json!([{ "name": "init", "description": "Create AGENTS.md", "source": "command" }]),
            )),
            first_prompt_had_subscriber: Arc::default(),
            fail_session_creates: Arc::default(),
            config: Arc::new(Mutex::new(json!({}))),
        };
        let accept = fake.clone();
        tokio::spawn(async move {
            while let Ok((stream, _)) = listener.accept().await {
                let fake = accept.clone();
                tokio::spawn(async move { fake.serve(stream).await });
            }
        });
        fake
    }

    fn emit(&self, payload: Value) {
        if payload["type"] == "session.status"
            && let Some(id) = payload["properties"]["sessionID"].as_str()
        {
            self.statuses
                .lock()
                .unwrap()
                .insert(id.to_owned(), payload["properties"]["status"].clone());
        }
        let framed = format!(
            "data: {}\n\n",
            json!({ "directory": "/", "payload": payload })
        );
        let mut backlog = self.backlog.lock().unwrap();
        let sequence = backlog.len() as u64;
        backlog.push((sequence, framed.clone()));
        self.events.send((sequence, framed)).ok();
    }

    fn set_providers(&self, providers: Value) {
        *self.providers.lock().unwrap() = providers;
    }

    fn posts_to(&self, path: &str) -> Vec<Value> {
        self.posts
            .lock()
            .unwrap()
            .iter()
            .filter(|(posted, _)| posted == path)
            .map(|(_, body)| body.clone())
            .collect()
    }

    async fn serve(self, mut stream: tokio::net::TcpStream) {
        let mut buffer: Vec<u8> = Vec::new();
        let mut chunk = [0u8; 4096];
        loop {
            let header_end = loop {
                if let Some(position) = buffer.windows(4).position(|w| w == b"\r\n\r\n") {
                    break position + 4;
                }
                match stream.read(&mut chunk).await {
                    Ok(0) | Err(_) => return,
                    Ok(read) => buffer.extend_from_slice(&chunk[..read]),
                }
            };
            let head = String::from_utf8_lossy(&buffer[..header_end]).to_string();
            let mut lines = head.lines();
            let start = lines.next().unwrap_or_default().to_owned();
            let content_length = lines
                .filter_map(|line| {
                    let (key, value) = line.split_once(':')?;
                    key.eq_ignore_ascii_case("content-length")
                        .then(|| value.trim().parse::<usize>().ok())
                        .flatten()
                })
                .next()
                .unwrap_or(0);
            while buffer.len() < header_end + content_length {
                match stream.read(&mut chunk).await {
                    Ok(0) | Err(_) => return,
                    Ok(read) => buffer.extend_from_slice(&chunk[..read]),
                }
            }
            let body: Value =
                serde_json::from_slice(&buffer[header_end..header_end + content_length])
                    .unwrap_or(Value::Null);
            buffer.drain(..header_end + content_length);

            let mut parts = start.split_whitespace();
            let method = parts.next().unwrap_or_default().to_owned();
            let target = parts.next().unwrap_or_default().to_owned();
            let path = target.split('?').next().unwrap_or_default().to_owned();

            if method == "GET" && path == "/global/event" {
                // Subscribe before the backlog snapshot; overlapping frames dedupe by sequence.
                let mut receiver = self.events.subscribe();
                let replay = self.backlog.lock().unwrap().clone();
                let mut next_sequence = replay.len() as u64;
                stream
                    .write_all(
                        b"HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\n\
                          cache-control: no-cache\r\nconnection: close\r\n\r\n",
                    )
                    .await
                    .ok();
                stream
                    .write_all(b"data: {\"payload\":{\"type\":\"server.connected\",\"properties\":{}}}\n\n")
                    .await
                    .ok();
                for (_, frame) in &replay {
                    if stream.write_all(frame.as_bytes()).await.is_err() {
                        return;
                    }
                }
                stream.flush().await.ok();
                while let Ok((sequence, frame)) = receiver.recv().await {
                    if sequence < next_sequence {
                        continue;
                    }
                    next_sequence = sequence + 1;
                    if stream.write_all(frame.as_bytes()).await.is_err() {
                        return;
                    }
                    stream.flush().await.ok();
                }
                return;
            }

            if method == "POST" || method == "PATCH" {
                if path.ends_with("/prompt_async") {
                    let mut first = self.first_prompt_had_subscriber.lock().unwrap();
                    if first.is_none() {
                        *first = Some(self.events.receiver_count() > 0);
                    }
                }
                self.posts.lock().unwrap().push((path.clone(), body));
            }
            let (status, payload) = self.route(&method, &path);
            let body = payload.to_string();
            let response = format!(
                "HTTP/1.1 {status}\r\ncontent-type: application/json\r\n\
                 content-length: {}\r\n\r\n{body}",
                body.len()
            );
            if stream.write_all(response.as_bytes()).await.is_err() {
                return;
            }
        }
    }

    fn route(&self, method: &str, path: &str) -> (&'static str, Value) {
        match (method, path) {
            ("GET", "/global/health") => {
                ("200 OK", json!({ "healthy": true, "version": "1.18.20" }))
            }
            ("GET", "/provider") => ("200 OK", self.providers.lock().unwrap().clone()),
            ("GET", "/command") => ("200 OK", self.commands.lock().unwrap().clone()),
            ("GET", "/config") => ("200 OK", self.config.lock().unwrap().clone()),
            ("GET", "/agent") => (
                "200 OK",
                json!([
                    {"name": "explore", "mode": "subagent", "permission": [
                        {"permission": "*", "pattern": "*", "action": "deny"}
                    ]},
                    {"name": "build", "mode": "primary", "permission": [
                        {"permission": "*", "pattern": "*", "action": "allow"},
                        {"permission": "bash", "pattern": "rm *", "action": "deny"}
                    ]}
                ]),
            ),
            ("PATCH", path) if path.starts_with("/session/") => ("200 OK", json!({})),
            ("POST", "/session") => {
                let mut fails = self.fail_session_creates.lock().unwrap();
                if *fails > 0 {
                    *fails -= 1;
                    (
                        "500 Internal Server Error",
                        json!({
                            "name": "UnknownError",
                            "data": {
                                "message": "Unexpected server error. Check server logs for details.",
                                "ref": "err_test",
                            },
                        }),
                    )
                } else {
                    ("200 OK", json!({ "id": "ses_test" }))
                }
            }
            ("GET", "/session/status") => (
                "200 OK",
                Value::Object(self.statuses.lock().unwrap().clone()),
            ),
            ("GET", "/session/ses_resume") => ("200 OK", json!({ "id": "ses_resume" })),
            ("GET", path) if path.starts_with("/session/") => ("404 Not Found", json!({})),
            ("POST", "/session/ses_resume/fork") => ("200 OK", json!({ "id": "ses_resume" })),
            ("POST", path) if path.ends_with("/prompt_async") => ("204 No Content", json!({})),
            ("POST", path) if path.ends_with("/abort") => ("200 OK", json!(true)),
            ("POST", path) if path.contains("/permission/") || path.contains("/question/") => {
                ("200 OK", json!(true))
            }
            _ => ("404 Not Found", json!({ "missing": path })),
        }
    }
}

fn request(prompt: &str) -> RunRequest {
    RunRequest {
        prompt: prompt.into(),
        model: None,
        reasoning: None,
        model_options: serde_json::Map::new(),
        cwd: "/tmp".into(),
        permission: PermissionMode::FullAccess,
        resume: None,
        fork: None,
        attachments: Vec::new(),
        skills: Vec::new(),
        auto_compact_tokens: None,
    }
}

type Answers = Arc<Mutex<Vec<String>>>;

fn controls_answering(
    label: Option<&'static str>,
) -> (
    RunControls,
    mpsc::Sender<SteerMessage>,
    CancellationToken,
    Answers,
) {
    let (steer_tx, steering) = mpsc::channel(8);
    let token = CancellationToken::new();
    let asked: Answers = Arc::default();
    let recorded = asked.clone();
    let controls = RunControls {
        request_input: Box::new(move |questions| {
            let (tx, rx) = oneshot::channel();
            let answers: Vec<UserInputAnswer> = questions
                .iter()
                .map(|question| {
                    recorded.lock().unwrap().push(question.question.clone());
                    UserInputAnswer {
                        question_id: question.id.clone(),
                        labels: label
                            .map(str::to_owned)
                            .or_else(|| question.options.first().cloned())
                            .into_iter()
                            .collect(),
                    }
                })
                .collect();
            tx.send(answers).ok();
            rx
        }),
        steering,
        interrupt: token.clone(),
    };
    (controls, steer_tx, token, asked)
}

fn controls() -> (RunControls, mpsc::Sender<SteerMessage>, CancellationToken) {
    let (controls, steer, token, _) = controls_answering(None);
    (controls, steer, token)
}

fn harness(fake: &FakeOpencode) -> OpencodeHarness {
    OpencodeHarness::new().with_base_url(fake.base.clone())
}

fn assistant_message(fake: &FakeOpencode, session: &str, message: &str) {
    fake.emit(json!({
        "type": "session.status",
        "properties": { "sessionID": session, "status": { "type": "busy" } },
    }));
    fake.emit(json!({
        "type": "message.updated",
        "properties": { "info": { "id": message, "role": "assistant", "sessionID": session } },
    }));
}

fn idle(fake: &FakeOpencode, session: &str) {
    fake.emit(json!({
        "type": "session.status",
        "properties": { "sessionID": session, "status": { "type": "idle" } },
    }));
}

async fn next_event(
    stream: &mut (impl futures::Stream<Item = Result<AgentEvent, HarnessError>> + Unpin),
) -> AgentEvent {
    tokio::time::timeout(Duration::from_secs(10), stream.next())
        .await
        .expect("event within budget")
        .expect("stream open")
        .expect("ok event")
}

async fn wait_posts(fake: &FakeOpencode, path: &str, count: usize) -> Vec<Value> {
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let posts = fake.posts_to(path);
            if posts.len() >= count {
                return posts;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap_or_else(|_| panic!("{path} never saw {count} posts"))
}

async fn drain_to_done(
    stream: &mut (impl futures::Stream<Item = Result<AgentEvent, HarnessError>> + Unpin),
) -> Vec<AgentEvent> {
    let mut events = Vec::new();
    loop {
        let event = next_event(stream).await;
        let done = matches!(&event, AgentEvent::Done { .. });
        events.push(event);
        if done {
            return events;
        }
    }
}

#[tokio::test]
async fn thinking_streams_and_the_turn_settles_only_on_idle() {
    let fake = FakeOpencode::start().await;
    let (controls, _steer, _token) = controls();
    let mut stream = harness(&fake).run(request("hi"), controls).await.unwrap();

    let started = next_event(&mut stream).await;
    assert!(matches!(
        &started,
        AgentEvent::SessionStarted { session_id, .. } if session_id == "ses_test"
    ));
    let commands = next_event(&mut stream).await;
    assert!(matches!(
        &commands,
        AgentEvent::AvailableCommands { commands } if commands.len() == 1
    ));

    assistant_message(&fake, "ses_test", "msg_1");
    fake.emit(json!({
        "type": "message.part.updated",
        "properties": { "part": {
            "id": "prt_r", "messageID": "msg_1", "sessionID": "ses_test",
            "type": "reasoning", "text": "",
        }},
    }));
    fake.emit(json!({
        "type": "message.part.delta",
        "properties": {
            "sessionID": "ses_test", "messageID": "msg_1", "partID": "prt_r",
            "field": "text", "delta": "let me think",
        },
    }));
    let thinking = next_event(&mut stream).await;
    assert!(matches!(
        &thinking,
        AgentEvent::ReasoningDelta { text } if text == "let me think"
    ));
    fake.emit(json!({
        "type": "message.part.updated",
        "properties": { "part": {
            "id": "prt_r", "messageID": "msg_1", "sessionID": "ses_test",
            "type": "reasoning", "text": "let me think",
        }},
    }));
    fake.emit(json!({
        "type": "message.part.updated",
        "properties": { "part": {
            "id": "prt_t", "messageID": "msg_1", "sessionID": "ses_test",
            "type": "text", "text": "Hello",
        }},
    }));
    let text = next_event(&mut stream).await;
    assert!(matches!(&text, AgentEvent::TextDelta { text } if text == "Hello"));
    let quiet = tokio::time::timeout(Duration::from_millis(600), stream.next()).await;
    assert!(quiet.is_err(), "nothing may settle a quiet but live turn");

    idle(&fake, "ses_test");
    let events = drain_to_done(&mut stream).await;
    assert!(matches!(
        events.first(),
        Some(AgentEvent::AssistantMessageCompleted { .. })
    ));
    assert!(matches!(
        events.last(),
        Some(AgentEvent::Done {
            status: DoneStatus::Completed,
            session_id: Some(session),
            ..
        }) if session == "ses_test"
    ));
}

#[tokio::test]
async fn session_create_retries_once_through_the_lazy_migration_500() {
    let fake = FakeOpencode::start().await;
    *fake.fail_session_creates.lock().unwrap() = 1;
    let (controls, _steer, _token) = controls();
    let mut stream = harness(&fake).run(request("hi"), controls).await.unwrap();
    let started = next_event(&mut stream).await;
    assert!(matches!(
        &started,
        AgentEvent::SessionStarted { session_id, .. } if session_id == "ses_test"
    ));
    assert!(matches!(
        next_event(&mut stream).await,
        AgentEvent::AvailableCommands { .. }
    ));
    assert_eq!(wait_posts(&fake, "/session", 2).await.len(), 2);
    assistant_message(&fake, "ses_test", "msg_1");
    idle(&fake, "ses_test");
    let events = drain_to_done(&mut stream).await;
    assert!(
        !events
            .iter()
            .any(|event| matches!(event, AgentEvent::Error { .. }))
    );
    assert!(matches!(
        events.last(),
        Some(AgentEvent::Done {
            status: DoneStatus::Completed,
            ..
        })
    ));
}

#[tokio::test]
async fn foreign_session_idle_never_settles_our_turn() {
    let fake = FakeOpencode::start().await;
    let (controls, _steer, _token) = controls();
    let mut stream = harness(&fake).run(request("hi"), controls).await.unwrap();
    next_event(&mut stream).await;
    next_event(&mut stream).await;

    assistant_message(&fake, "ses_test", "msg_1");
    idle(&fake, "ses_OTHER");
    let quiet = tokio::time::timeout(Duration::from_millis(600), stream.next()).await;
    assert!(quiet.is_err(), "a foreign session's idle settled our turn");

    idle(&fake, "ses_test");
    let events = drain_to_done(&mut stream).await;
    assert!(matches!(
        events.last(),
        Some(AgentEvent::Done {
            status: DoneStatus::Completed,
            ..
        })
    ));
}

#[tokio::test]
async fn model_and_advertised_variant_ride_the_prompt() {
    let fake = FakeOpencode::start().await;
    fake.set_providers(json!({
        "all": [{
            "id": "anthropic",
            "name": "Anthropic",
            "models": { "opus": { "name": "Opus", "variants": { "high": {}, "max": {} } } },
        }],
    }));
    let (controls, _steer, _token) = controls();
    let mut run = request("hi");
    run.model = Some("anthropic/opus".into());
    run.reasoning = Some(ReasoningLevel::XHigh);
    run.attachments.push("/tmp/shot.png".into());
    let mut stream = harness(&fake).run(run, controls).await.unwrap();
    next_event(&mut stream).await;
    next_event(&mut stream).await;

    let prompts = wait_posts(&fake, "/session/ses_test/prompt_async", 1).await;
    assert_eq!(prompts[0]["model"]["providerID"], "anthropic");
    assert_eq!(prompts[0]["model"]["modelID"], "opus");
    assert_eq!(prompts[0]["variant"], "high");
    assert_eq!(prompts[0]["parts"][0]["text"], "hi");
    assert_eq!(prompts[0]["parts"][1]["url"], "file:///tmp/shot.png");

    assistant_message(&fake, "ses_test", "msg_1");
    idle(&fake, "ses_test");
    drain_to_done(&mut stream).await;
}

#[tokio::test]
async fn steer_queues_mid_turn_and_delivers_at_idle() {
    let fake = FakeOpencode::start().await;
    let (controls, steer, _token) = controls();
    let mut stream = harness(&fake).run(request("hi"), controls).await.unwrap();
    next_event(&mut stream).await;
    next_event(&mut stream).await;

    assistant_message(&fake, "ses_test", "msg_1");
    fake.emit(json!({
        "type": "message.part.updated",
        "properties": { "part": {
            "id": "prt_t", "messageID": "msg_1", "sessionID": "ses_test",
            "type": "text", "text": "working",
        }},
    }));
    next_event(&mut stream).await;

    steer
        .send(SteerMessage {
            prompt: "also do this".into(),
            message_id: None,
            attachments: vec!["/tmp/extra.png".into()],
            skills: Vec::new(),
        })
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(100)).await;
    idle(&fake, "ses_test");

    let event = next_event(&mut stream).await;
    assert!(
        matches!(&event, AgentEvent::Steered { .. }),
        "queued steer must continue the run at the turn boundary, got {event:?}"
    );
    let prompts = wait_posts(&fake, "/session/ses_test/prompt_async", 2).await;
    assert_eq!(prompts[1]["parts"][0]["text"], "also do this");
    assert_eq!(prompts[1]["parts"][1]["url"], "file:///tmp/extra.png");

    assistant_message(&fake, "ses_test", "msg_2");
    idle(&fake, "ses_test");
    let events = drain_to_done(&mut stream).await;
    assert!(matches!(
        events.last(),
        Some(AgentEvent::Done {
            status: DoneStatus::Completed,
            ..
        })
    ));
}

#[tokio::test]
async fn interrupt_aborts_and_settles_interrupted() {
    let fake = FakeOpencode::start().await;
    let (controls, _steer, token) = controls();
    let mut stream = harness(&fake).run(request("hi"), controls).await.unwrap();
    next_event(&mut stream).await;
    next_event(&mut stream).await;

    assistant_message(&fake, "ses_test", "msg_1");
    token.cancel();
    wait_posts(&fake, "/session/ses_test/abort", 1).await;
    idle(&fake, "ses_test");
    let events = drain_to_done(&mut stream).await;
    assert!(matches!(
        events.last(),
        Some(AgentEvent::Done {
            status: DoneStatus::Interrupted,
            ..
        })
    ));
}

#[tokio::test]
async fn provider_retries_surface_and_cap_out() {
    let fake = FakeOpencode::start().await;
    let (controls, _steer, _token) = controls();
    let mut stream = harness(&fake).run(request("hi"), controls).await.unwrap();
    next_event(&mut stream).await;
    next_event(&mut stream).await;

    let retry = |attempt: u64| {
        json!({
            "type": "session.status",
            "properties": { "sessionID": "ses_test", "status": {
                "type": "retry", "attempt": attempt,
                "message": "AI_APICallError: unreachable", "next": 0,
            }},
        })
    };
    fake.emit(retry(1));
    fake.emit(retry(3));
    let AgentEvent::Error { message } = next_event(&mut stream).await else {
        panic!("expected a retry error chip");
    };
    assert!(
        message.contains("retrying") && message.contains("attempt 3"),
        "{message}"
    );
    assert!(message.contains("unreachable"), "{message}");

    fake.emit(retry(8));
    let AgentEvent::Error { message } = next_event(&mut stream).await else {
        panic!("expected the give-up chip");
    };
    assert!(message.contains("Giving up"), "{message}");
    wait_posts(&fake, "/session/ses_test/abort", 1).await;
    idle(&fake, "ses_test");
    let events = drain_to_done(&mut stream).await;
    assert!(matches!(
        events.last(),
        Some(AgentEvent::Done {
            status: DoneStatus::Errored,
            error: Some(_),
            ..
        })
    ));
}

#[tokio::test]
async fn session_error_with_no_content_settles_errored() {
    let fake = FakeOpencode::start().await;
    let (controls, _steer, _token) = controls();
    let mut stream = harness(&fake).run(request("hi"), controls).await.unwrap();
    next_event(&mut stream).await;
    next_event(&mut stream).await;

    fake.emit(json!({
        "type": "session.error",
        "properties": { "sessionID": "ses_test", "error": {
            "name": "ProviderAuthError",
            "data": { "message": "no credentials for anthropic" },
        }},
    }));
    assert!(matches!(
        next_event(&mut stream).await,
        AgentEvent::Error { message } if message.contains("no credentials")
    ));
    fake.emit(json!({
        "type": "session.error",
        "properties": { "sessionID": "ses_test", "error": {
            "name": "UnknownError",
            "data": { "message": "ProviderAuthError: no credentials for anthropic\n    at stack" },
        }},
    }));
    let quiet = tokio::time::timeout(Duration::from_millis(400), stream.next()).await;
    assert!(
        quiet.is_err(),
        "duplicate error must not mint a second chip"
    );
    idle(&fake, "ses_test");
    let events = drain_to_done(&mut stream).await;
    assert!(matches!(
        events.last(),
        Some(AgentEvent::Done {
            status: DoneStatus::Errored,
            error: Some(error),
            ..
        }) if error.contains("no credentials")
    ));
}

#[tokio::test]
async fn subagent_task_streams_tagged_and_settles_from_the_task_part() {
    let fake = FakeOpencode::start().await;
    let (controls, _steer, _token) = controls();
    let mut stream = harness(&fake)
        .run(request("spawn"), controls)
        .await
        .unwrap();
    next_event(&mut stream).await;
    next_event(&mut stream).await;

    assistant_message(&fake, "ses_test", "msg_1");
    fake.emit(json!({
        "type": "message.part.updated",
        "properties": { "part": {
            "id": "prt_task", "messageID": "msg_1", "sessionID": "ses_test",
            "type": "tool", "tool": "task",
            "state": {
                "status": "running",
                "input": { "description": "Viz probe", "prompt": "run", "subagent_type": "general" },
                "metadata": { "sessionId": "ses_child", "parentSessionId": "ses_test" },
            },
        }},
    }));
    assert!(matches!(
        next_event(&mut stream).await,
        AgentEvent::ToolCall { id, call: ToolCall::Unknown { name, .. } }
            if id == "prt_task" && name == "Agent: Viz probe"
    ));

    fake.emit(json!({
        "type": "session.created",
        "properties": { "info": {
            "id": "ses_child", "parentID": "ses_test",
            "title": "Viz probe (@general subagent)",
        }},
    }));
    fake.emit(json!({
        "type": "message.updated",
        "properties": { "info": { "id": "msg_cu", "role": "user", "sessionID": "ses_child" } },
    }));
    fake.emit(json!({
        "type": "message.part.updated",
        "properties": { "part": {
            "id": "prt_cu", "messageID": "msg_cu", "sessionID": "ses_child",
            "type": "text", "text": "run",
        }},
    }));
    assert!(matches!(
        next_event(&mut stream).await,
        AgentEvent::Subagent { parent_tool_use_id, event }
            if parent_tool_use_id == "prt_task"
                && matches!(&*event, AgentEvent::UserMessage { text } if text == "run")
    ));
    fake.emit(json!({
        "type": "message.updated",
        "properties": { "info": { "id": "msg_ca", "role": "assistant", "sessionID": "ses_child" } },
    }));
    fake.emit(json!({
        "type": "message.part.updated",
        "properties": { "part": {
            "id": "prt_ca", "messageID": "msg_ca", "sessionID": "ses_child",
            "type": "text", "text": "finished",
        }},
    }));
    assert!(matches!(
        next_event(&mut stream).await,
        AgentEvent::Subagent { event, .. }
            if matches!(&*event, AgentEvent::TextDelta { text } if text == "finished")
    ));

    fake.emit(json!({
        "type": "message.part.updated",
        "properties": { "part": {
            "id": "prt_task", "messageID": "msg_1", "sessionID": "ses_test",
            "type": "tool", "tool": "task",
            "state": {
                "status": "completed",
                "input": { "description": "Viz probe" },
                "output": "<task_result>finished</task_result>",
                "metadata": { "sessionId": "ses_child" },
            },
        }},
    }));
    assert!(matches!(
        next_event(&mut stream).await,
        AgentEvent::ToolResult { id, is_error: false, .. } if id == "prt_task"
    ));
    assert!(matches!(
        next_event(&mut stream).await,
        AgentEvent::Subagent { parent_tool_use_id, event }
            if parent_tool_use_id == "prt_task"
                && matches!(&*event, AgentEvent::Done { status: DoneStatus::Completed, .. })
    ));

    idle(&fake, "ses_test");
    let events = drain_to_done(&mut stream).await;
    assert!(matches!(
        events.last(),
        Some(AgentEvent::Done {
            status: DoneStatus::Completed,
            ..
        })
    ));
}

#[tokio::test]
async fn resume_reuses_the_durable_session() {
    let fake = FakeOpencode::start().await;
    let (controls, _steer, _token) = controls();
    let mut run = request("continue");
    run.resume = Some("ses_resume".into());
    let mut stream = harness(&fake).run(run, controls).await.unwrap();
    assert!(matches!(
        next_event(&mut stream).await,
        AgentEvent::SessionStarted { session_id, .. } if session_id == "ses_resume"
    ));
    next_event(&mut stream).await;
    wait_posts(&fake, "/session/ses_resume/prompt_async", 1).await;
    assistant_message(&fake, "ses_resume", "msg_1");
    idle(&fake, "ses_resume");
    drain_to_done(&mut stream).await;
}

#[tokio::test]
async fn fork_continues_in_the_copy_opencode_makes() {
    let fake = FakeOpencode::start().await;
    let (unforkable_controls, _steer, _token) = controls();
    let mut run = request("continue");
    run.fork = Some("ses_parent".into());
    let mut stream = harness(&fake).run(run, unforkable_controls).await.unwrap();
    assert!(
        matches!(
            next_event(&mut stream).await,
            AgentEvent::Error { .. }
                | AgentEvent::Done {
                    status: DoneStatus::Errored,
                    ..
                }
        ),
        "a session opencode can't fork fails instead of silently starting fresh"
    );

    let fake = FakeOpencode::start().await;
    let (controls, _steer, _token) = controls();
    let mut run = request("continue");
    run.fork = Some("ses_resume".into());
    let mut stream = harness(&fake).run(run, controls).await.unwrap();
    assert!(matches!(
        next_event(&mut stream).await,
        AgentEvent::SessionStarted { session_id, .. } if session_id == "ses_resume"
    ));
    assert_eq!(fake.posts_to("/session/ses_resume/fork").len(), 1);
    assert!(
        fake.posts_to("/session").is_empty(),
        "no fresh session is created"
    );
}

#[tokio::test]
async fn slash_command_routes_through_the_command_endpoint() {
    let fake = FakeOpencode::start().await;
    let (controls, _steer, _token) = controls();
    let mut stream = harness(&fake)
        .run(request("/init the repo"), controls)
        .await
        .unwrap();
    next_event(&mut stream).await;
    next_event(&mut stream).await;

    let commands = wait_posts(&fake, "/session/ses_test/command", 1).await;
    assert_eq!(commands[0]["command"], "init");
    assert_eq!(commands[0]["arguments"], "the repo");
    assert!(fake.posts_to("/session/ses_test/prompt_async").is_empty());

    assistant_message(&fake, "ses_test", "msg_1");
    idle(&fake, "ses_test");
    drain_to_done(&mut stream).await;
}

#[tokio::test]
async fn unknown_slash_text_stays_an_ordinary_prompt() {
    let fake = FakeOpencode::start().await;
    let (controls, _steer, _token) = controls();
    let mut stream = harness(&fake)
        .run(request("/usr/bin is missing"), controls)
        .await
        .unwrap();
    next_event(&mut stream).await;
    next_event(&mut stream).await;
    let prompts = wait_posts(&fake, "/session/ses_test/prompt_async", 1).await;
    assert_eq!(prompts[0]["parts"][0]["text"], "/usr/bin is missing");
    assistant_message(&fake, "ses_test", "msg_1");
    idle(&fake, "ses_test");
    drain_to_done(&mut stream).await;
}

#[tokio::test]
async fn first_prompt_waits_for_the_live_event_subscription() {
    let fake = FakeOpencode::start().await;
    let (controls, _steer, _token) = controls();
    let mut stream = harness(&fake).run(request("hi"), controls).await.unwrap();
    next_event(&mut stream).await;
    next_event(&mut stream).await;
    wait_posts(&fake, "/session/ses_test/prompt_async", 1).await;
    assert_eq!(
        *fake.first_prompt_had_subscriber.lock().unwrap(),
        Some(true),
        "prompt must not be posted before the /global/event subscription exists"
    );

    fake.emit(json!({
        "type": "session.status",
        "properties": { "sessionID": "ses_test", "status": { "type": "busy" } },
    }));
    fake.emit(json!({
        "type": "session.error",
        "properties": { "sessionID": "ses_test", "error": {
            "name": "UnknownError",
            "data": { "message": "Model not found: opencode/gone-model" },
        }},
    }));
    idle(&fake, "ses_test");
    let events = drain_to_done(&mut stream).await;
    assert!(matches!(
        events.last(),
        Some(AgentEvent::Done {
            status: DoneStatus::Errored,
            error: Some(error),
            ..
        }) if error.contains("Model not found")
    ));
}

#[tokio::test]
async fn models_discover_from_the_provider_catalog() {
    let fake = FakeOpencode::start().await;
    fake.set_providers(json!({
        "all": [
            {
                "id": "opencode",
                "name": "OpenCode Zen",
                "models": { "big-pickle": { "name": "Big Pickle" } },
            },
            {
                "id": "catalog-only",
                "name": "Needs A Key",
                "models": { "locked": { "name": "Locked" } },
            },
        ],
        "connected": ["opencode"],
    }));
    let harness = harness(&fake);
    let models = harness.models().await.unwrap();
    assert_eq!(models.len(), 1);
    assert_eq!(models[0].id, "opencode/big-pickle");
    assert!(
        models[0].options.is_empty(),
        "1.x must not advertise agents"
    );
}

#[tokio::test]
async fn models_keep_large_catalog_on_empty_response_and_recover() {
    let fake = FakeOpencode::start().await;
    let harness = harness(&fake);
    let models: serde_json::Map<String, Value> = (0..512)
        .map(|index| (format!("model-{index}"), json!({"name": "x".repeat(2048)})))
        .collect();
    fake.set_providers(json!({
        "all": [{"id": "provider", "models": models}],
        "connected": ["provider"],
    }));
    assert_eq!(harness.model_catalog(true).await.unwrap().models.len(), 512);

    fake.set_providers(json!({"all": [], "connected": []}));
    let retained = harness.model_catalog(true).await.unwrap().models;
    assert_eq!(retained.len(), 512);
    assert!(
        retained
            .iter()
            .all(|model| model.id.starts_with("provider/"))
    );

    fake.set_providers(json!({
        "all": [{"id": "new-account", "models": {"fresh": {"name": "Fresh"}}}],
        "connected": ["new-account"],
    }));
    let refreshed = harness.model_catalog(true).await.unwrap().models;
    assert_eq!(refreshed.len(), 1);
    assert_eq!(refreshed[0].id, "new-account/fresh");
}

#[tokio::test]
async fn repeated_session_create_failure_stops_after_one_retry() {
    let fake = FakeOpencode::start().await;
    *fake.fail_session_creates.lock().unwrap() = 10;
    let (controls, _, _) = controls();
    let mut stream = harness(&fake).run(request("hi"), controls).await.unwrap();
    let events = drain_to_done(&mut stream).await;
    assert_eq!(fake.posts_to("/session").len(), 2);
    assert!(
        !events
            .iter()
            .any(|event| matches!(event, AgentEvent::SessionStarted { .. }))
    );
    assert!(matches!(
        events.last(),
        Some(AgentEvent::Done {
            status: DoneStatus::Errored,
            ..
        })
    ));
}

#[tokio::test]
async fn slash_command_rejects_attachments_instead_of_dropping_them() {
    let fake = FakeOpencode::start().await;
    let (controls, _steer, _token) = controls();
    let mut run = request("/init the repo");
    run.attachments.push("/tmp/image.png".into());
    let mut stream = harness(&fake).run(run, controls).await.unwrap();
    let events = drain_to_done(&mut stream).await;
    assert!(events.iter().any(|event| matches!(event,
        AgentEvent::Done { status: DoneStatus::Errored, error: Some(message), .. }
        if message.contains("attachments")
    )));
    assert!(fake.posts_to("/session/ses_test/command").is_empty());
    assert!(fake.posts_to("/session/ses_test/prompt_async").is_empty());
}

#[tokio::test]
async fn commands_and_skills_come_from_the_project_catalog() {
    let fake = FakeOpencode::start().await;
    *fake.commands.lock().unwrap() = json!([
        {"name": "review", "description": "Native skill", "source": "skill"},
        {"name": "init", "description": "Create AGENTS.md", "source": "command"}
    ]);
    let project = tempfile::tempdir().unwrap();
    let harness = harness(&fake);
    let commands = harness.commands(project.path()).await.unwrap();
    assert_eq!(commands.len(), 1);
    assert_eq!(commands[0].name, "init");
    let skills = harness.skills(project.path()).await.unwrap();
    assert_eq!(skills.len(), 1);
    assert_eq!(skills[0].name, "review");
    assert_eq!(skills[0].path.as_deref(), Some("opencode-skill:review"));
}

#[tokio::test]
async fn dollar_selected_skill_uses_opencode_native_command_with_arguments() {
    let fake = FakeOpencode::start().await;
    *fake.commands.lock().unwrap() = json!([
        {"name": "review", "description": "Native skill", "source": "skill"},
        {"name": "init", "source": "command"}
    ]);
    let project = tempfile::tempdir().unwrap();
    let harness = harness(&fake);
    let skill = harness
        .skills(project.path())
        .await
        .unwrap()
        .into_iter()
        .find(|skill| skill.name == "review")
        .unwrap();
    let (controls, _steer, _token) = controls();
    let mut run = request("\n  $review inspect tests");
    run.skills.push(SkillRef {
        name: skill.name,
        path: skill.path.unwrap(),
    });
    let mut stream = harness.run(run, controls).await.unwrap();
    next_event(&mut stream).await;
    next_event(&mut stream).await;
    let commands = wait_posts(&fake, "/session/ses_test/command", 1).await;
    assert_eq!(commands[0]["command"], "review");
    assert_eq!(commands[0]["arguments"], "inspect tests");
    assert!(fake.posts_to("/session/ses_test/prompt_async").is_empty());
    assistant_message(&fake, "ses_test", "msg_1");
    idle(&fake, "ses_test");
    drain_to_done(&mut stream).await;
}

async fn ask_permission(
    mode: PermissionMode,
    kind: &str,
    label: Option<&'static str>,
) -> (Vec<AgentEvent>, Vec<Value>, Vec<String>) {
    let fake = FakeOpencode::start().await;
    let (controls, _steer, _token, asked) = controls_answering(label);
    let mut run = request("hi");
    run.permission = mode;
    let mut stream = harness(&fake).run(run, controls).await.unwrap();
    next_event(&mut stream).await;
    next_event(&mut stream).await;
    assistant_message(&fake, "ses_test", "msg_1");
    fake.emit(json!({
        "type": "permission.asked",
        "properties": {
            "id": "per_1", "sessionID": "ses_test", "permission": kind,
            "patterns": ["src/main.rs"], "always": ["*"],
            "metadata": { "filepath": "/tmp/src/main.rs", "command": "make" },
        },
    }));
    let replies = wait_posts(&fake, "/permission/per_1/reply", 1).await;
    let mut events = Vec::new();
    if !matches!(
        (mode, kind),
        (PermissionMode::FullAccess, _) | (PermissionMode::AcceptEdits, "edit")
    ) {
        loop {
            let event = next_event(&mut stream).await;
            let resolved = matches!(&event, AgentEvent::InputResolved { .. });
            events.push(event);
            if resolved {
                break;
            }
        }
    }
    idle(&fake, "ses_test");
    events.extend(drain_to_done(&mut stream).await);
    let asked = asked.lock().unwrap().clone();
    (events, replies, asked)
}

#[tokio::test]
async fn asking_modes_ask_the_user_and_reply_once_or_reject() {
    for (mode, kind, question) in [
        (PermissionMode::Ask, "edit", "Edit /tmp/src/main.rs?"),
        (PermissionMode::Ask, "bash", "Run `make`?"),
        (PermissionMode::AcceptEdits, "bash", "Run `make`?"),
        (PermissionMode::Auto, "edit", "Edit /tmp/src/main.rs?"),
    ] {
        for (label, reply) in [(PERMISSION_ALLOW, "once"), (PERMISSION_DENY, "reject")] {
            let (events, replies, asked) = ask_permission(mode, kind, Some(label)).await;
            assert_eq!(
                replies,
                vec![json!({ "reply": reply })],
                "{mode:?} {kind} {label}"
            );
            assert_eq!(asked, vec![question.to_owned()]);
            let requested = events.iter().find_map(|event| match event {
                AgentEvent::InputRequested {
                    request_id,
                    questions,
                } => Some((request_id.clone(), questions.clone())),
                _ => None,
            });
            let (request_id, questions) = requested.expect("InputRequested");
            assert_eq!(request_id, "per_1");
            assert_eq!(questions[0].header, PERMISSION_HEADER);
            assert_eq!(
                questions[0].options,
                vec![PERMISSION_ALLOW, PERMISSION_DENY]
            );
            assert!(events.iter().any(|event| matches!(
                event,
                AgentEvent::InputResolved { request_id } if request_id == "per_1"
            )));
        }
    }
}

#[tokio::test]
async fn any_answer_but_allow_rejects() {
    let (_, replies, _) = ask_permission(PermissionMode::Ask, "edit", Some("Something else")).await;
    assert_eq!(replies, vec![json!({ "reply": "reject" })]);
}

#[tokio::test]
async fn full_access_replies_once_without_asking() {
    for (mode, kind) in [
        (PermissionMode::FullAccess, "bash"),
        (PermissionMode::FullAccess, "edit"),
        (PermissionMode::AcceptEdits, "edit"),
    ] {
        let (events, replies, asked) = ask_permission(mode, kind, None).await;
        assert_eq!(replies, vec![json!({ "reply": "once" })], "{mode:?} {kind}");
        assert!(asked.is_empty());
        assert!(
            !events
                .iter()
                .any(|event| matches!(event, AgentEvent::InputRequested { .. }))
        );
    }
}

#[tokio::test]
async fn asking_modes_create_the_session_with_rules_from_the_primary_agent() {
    for (mode, edit) in [
        (PermissionMode::Ask, "ask"),
        (PermissionMode::AcceptEdits, "allow"),
    ] {
        let fake = FakeOpencode::start().await;
        let (controls, _steer, _token) = controls();
        let mut run = request("hi");
        run.permission = mode;
        let mut stream = harness(&fake).run(run, controls).await.unwrap();
        next_event(&mut stream).await;
        let rules = wait_posts(&fake, "/session", 1).await[0]["permission"].clone();
        let rules = rules.as_array().unwrap();
        assert!(rules.contains(&json!({"permission": "edit", "pattern": "*", "action": edit})));
        assert!(rules.contains(&json!({"permission": "bash", "pattern": "*", "action": "ask"})));
        assert!(
            rules.contains(&json!({"permission": "bash", "pattern": "rm *", "action": "deny"}))
        );
        assert!(rules.contains(&json!({"permission": "task", "pattern": "*", "action": "ask"})));
        assistant_message(&fake, "ses_test", "msg_1");
        idle(&fake, "ses_test");
        drain_to_done(&mut stream).await;
    }
}

#[tokio::test]
async fn opencodes_default_model_comes_first() {
    let fake = FakeOpencode::start().await;
    fake.set_providers(json!({
        "all": [{"id": "opencode", "models": {"a-model": {"name": "A"}, "big-pickle": {"name": "Big Pickle"}}}],
        "connected": ["opencode"],
        "default": {"opencode": "big-pickle"},
    }));
    let harness = harness(&fake);
    assert_eq!(harness.models().await.unwrap()[0].id, "opencode/big-pickle");
    *fake.config.lock().unwrap() = json!({"model": "opencode/a-model"});
    assert_eq!(
        harness.model_catalog(true).await.unwrap().models[0].id,
        "opencode/a-model"
    );
}

#[tokio::test]
async fn one_x_commands_use_the_chosen_model() {
    let fake = FakeOpencode::start().await;
    fake.set_providers(json!({
        "all": [{"id": "anthropic", "models": {"opus": {"variants": {"high": {}}}}}],
    }));
    let (controls, _steer, _token) = controls();
    let mut run = request("/init the repo");
    run.model = Some("anthropic/opus".into());
    run.reasoning = Some(ReasoningLevel::High);
    let mut stream = harness(&fake).run(run, controls).await.unwrap();
    next_event(&mut stream).await;
    let commands = wait_posts(&fake, "/session/ses_test/command", 1).await;
    assert_eq!(commands[0]["model"], "anthropic/opus");
    assert_eq!(commands[0]["variant"], "high");
    assistant_message(&fake, "ses_test", "msg_1");
    idle(&fake, "ses_test");
    drain_to_done(&mut stream).await;
}

#[tokio::test]
async fn skills_with_images_or_mid_text_become_a_plain_prompt() {
    let fake = FakeOpencode::start().await;
    *fake.commands.lock().unwrap() = json!([
        {"name": "review", "description": "Native skill", "source": "skill"}
    ]);
    let (controls, _steer, _token) = controls();
    let mut run = request("$review this screenshot");
    run.attachments.push("/tmp/shot.png".into());
    run.skills.push(SkillRef {
        name: "review".into(),
        path: "opencode-skill:review".into(),
    });
    let mut stream = harness(&fake).run(run, controls).await.unwrap();
    next_event(&mut stream).await;
    let prompts = wait_posts(&fake, "/session/ses_test/prompt_async", 1).await;
    let text = prompts[0]["parts"][0]["text"].as_str().unwrap();
    assert!(text.starts_with("$review this screenshot"), "{text}");
    assert!(text.contains("`skill` tool: `review`"), "{text}");
    assert_eq!(prompts[0]["parts"][1]["url"], "file:///tmp/shot.png");
    assert!(fake.posts_to("/session/ses_test/command").is_empty());
    assistant_message(&fake, "ses_test", "msg_1");
    idle(&fake, "ses_test");
    drain_to_done(&mut stream).await;
}

#[tokio::test]
#[ignore = "talks to the installed opencode and a live model provider"]
async fn live_opencode_turn() {
    let project = tempfile::tempdir().unwrap();
    let harness = OpencodeHarness::new();
    let (controls, steer, _token) = controls();
    let mut run = request("Reply with the single word ok and nothing else.");
    run.cwd = project.path().to_string_lossy().into_owned();
    run.model = Some(
        std::env::var("WU_OPENCODE_LIVE_MODEL").unwrap_or_else(|_| "opencode/big-pickle".into()),
    );
    run.permission = PermissionMode::Ask;
    let mut stream = harness.run(run, controls).await.unwrap();
    let mut text = String::new();
    let done = tokio::time::timeout(Duration::from_secs(180), async {
        while let Some(event) = stream.next().await {
            let event = event.unwrap();
            eprintln!("{event:?}");
            match event {
                AgentEvent::TextDelta { text: delta } => text.push_str(&delta),
                AgentEvent::Done { status, error, .. } => return (status, error),
                _ => {}
            }
        }
        panic!("stream ended without Done");
    })
    .await
    .unwrap();
    // Closing steering ends the run, which shuts `opencode serve` down gracefully.
    drop(steer);
    tokio::time::timeout(Duration::from_secs(30), async {
        while stream.next().await.is_some() {}
    })
    .await
    .unwrap();
    assert_eq!(done, (DoneStatus::Completed, None), "{text}");
    assert!(text.to_lowercase().contains("ok"), "{text}");
}

#[tokio::test]
#[ignore = "talks to the installed opencode and a live model provider"]
async fn live_ask_mode_asks_before_commands_and_subagents() {
    let project = tempfile::tempdir().unwrap();
    for (prompt, expected) in [
        (
            "Use your bash tool to run exactly: echo wu-live . Do nothing else.",
            "Run `echo wu-live",
        ),
        (
            "Use the task tool to launch a general subagent that runs `echo wu-sub` with its bash tool. Do not run bash yourself.",
            "Start a general subagent?",
        ),
    ] {
        let (controls, steer, _token, asked) = controls_answering(Some(PERMISSION_DENY));
        let mut run = request(prompt);
        run.cwd = project.path().to_string_lossy().into_owned();
        run.model = Some(
            std::env::var("WU_OPENCODE_LIVE_MODEL")
                .unwrap_or_else(|_| "opencode/big-pickle".into()),
        );
        run.permission = PermissionMode::Ask;
        let mut stream = OpencodeHarness::new().run(run, controls).await.unwrap();
        let done = tokio::time::timeout(Duration::from_secs(180), async {
            while let Some(event) = stream.next().await {
                let event = event.unwrap();
                eprintln!("{event:?}");
                if let AgentEvent::Done { status, .. } = event {
                    return status;
                }
            }
            panic!("stream ended without Done");
        })
        .await
        .unwrap();
        drop(steer);
        tokio::time::timeout(Duration::from_secs(30), async {
            while stream.next().await.is_some() {}
        })
        .await
        .unwrap();
        assert_eq!(done, DoneStatus::Completed);
        let first_question = asked.lock().unwrap().first().cloned().unwrap_or_default();
        assert!(first_question.starts_with(expected), "{first_question}");
    }
}

#[tokio::test]
#[ignore = "boots the installed opencode; run with fresh XDG_* folders"]
async fn live_first_discovery_survives_opencode_writing_its_config() {
    let catalog = OpencodeHarness::new().model_catalog(false).await.unwrap();
    assert_eq!(catalog.source, "live");
    assert!(!catalog.models.is_empty());
    eprintln!("first model: {}", catalog.models[0].id);
}
