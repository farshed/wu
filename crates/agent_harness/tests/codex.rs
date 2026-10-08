#![cfg(unix)]

use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use futures::StreamExt;
use tokio::sync::{mpsc, oneshot};

use agent_harness::{
    AgentEvent, DoneStatus, HarnessId, PermissionMode, ReasoningLevel, RunRequest, Skill, SkillRef,
    TodoItem, TodoStatus, ToolCall, UserInputAnswer, UserInputQuestion,
};
use agent_harness::{
    CancellationToken, CodexHarness, Harness, HarnessError, RunControls, SteerMessage,
};

fn fixture_path() -> PathBuf {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join("fake-codex.sh");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if let Err(error) = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755))
        {
            eprintln!("could not mark fake-codex.sh executable: {error}");
        }
    }
    path
}

fn harness() -> CodexHarness {
    CodexHarness::new().with_executable(fixture_path())
}

fn request(prompt: &str) -> RunRequest {
    RunRequest {
        prompt: prompt.into(),
        model: Some("gpt-5.6-sol".into()),
        reasoning: Some(ReasoningLevel::Ultra),
        model_options: serde_json::Map::new(),
        cwd: String::new(),
        permission: PermissionMode::FullAccess,
        resume: None,
        fork: None,
        attachments: Vec::new(),
        skills: Vec::new(),
        auto_compact_tokens: None,
    }
}

fn controls(
    answer_label: &'static str,
) -> (RunControls, mpsc::Sender<SteerMessage>, CancellationToken) {
    let (steer_tx, steer_rx) = mpsc::channel(8);
    let token = CancellationToken::new();
    let controls = RunControls {
        request_input: Box::new(move |questions| {
            let (tx, rx) = oneshot::channel();
            let answers: Vec<UserInputAnswer> = questions
                .iter()
                .map(|q| UserInputAnswer {
                    question_id: q.id.clone(),
                    labels: vec![answer_label.into()],
                })
                .collect();
            tx.send(answers).expect("receiver is returned alongside");
            rx
        }),
        steering: steer_rx,
        interrupt: token.clone(),
    };
    (controls, steer_tx, token)
}

async fn run_to_end(
    harness: &CodexHarness,
    req: RunRequest,
    controls: RunControls,
) -> Vec<AgentEvent> {
    let stream = harness.run(req, controls).await.expect("run starts");
    tokio::time::timeout(
        Duration::from_secs(10),
        stream.map(|r| r.expect("stream event")).collect::<Vec<_>>(),
    )
    .await
    .expect("run finished in time")
}

#[tokio::test]
async fn reasoning_preserves_summary_parts_and_item_boundaries_per_thread() {
    let (controls, _steer, _token) = controls("Yes");
    let events = run_to_end(&harness(), request("scenario:reasoning"), controls).await;
    let mut parent = String::new();
    let mut child = String::new();
    for event in &events {
        match event {
            AgentEvent::ReasoningDelta { text } => parent.push_str(text),
            AgentEvent::Subagent { event, .. } => {
                if let AgentEvent::ReasoningDelta { text } = event.as_ref() {
                    child.push_str(text);
                }
            }
            _ => {}
        }
    }
    assert_eq!(
        parent,
        "**Implementing file badges**\n\n**Preparing fixture screenshots**\n\nChecking the final result."
    );
    assert_eq!(child, "**Checking layout**\n\nInspecting the output panel.");
}

#[tokio::test]
async fn happy_path_maps_deltas_items_usage_and_done() {
    let (controls, _steer, _token) = controls("Yes");
    let mut req = request("scenario:happy");
    req.cwd = "/tmp".into();
    req.model_options.insert(
        "serviceTier".into(),
        serde_json::Value::String("fast".into()),
    );
    let events = run_to_end(&harness(), req, controls).await;

    let starts: Vec<_> = events
        .iter()
        .filter_map(|e| match e {
            AgentEvent::SessionStarted {
                harness,
                model,
                cwd,
                session_id,
                ..
            } => Some((harness, model, cwd, session_id)),
            _ => None,
        })
        .collect();
    assert_eq!(starts.len(), 1, "{events:?}");
    let (h, model, cwd, session_id) = starts[0];
    assert_eq!(*h, HarnessId::Codex);
    assert_eq!(model, "gpt-5.6-sol");
    assert_eq!(cwd, "/tmp");
    assert_eq!(session_id, "th-1");

    assert!(events.contains(&AgentEvent::TextDelta {
        text: "Hello".into()
    }));
    assert!(events.contains(&AgentEvent::ReasoningDelta {
        text: "thinking hard".into()
    }));
    assert!(events.contains(&AgentEvent::ReasoningDelta {
        text: "summary".into()
    }));

    assert_eq!(
        events
            .iter()
            .filter(|e| matches!(e, AgentEvent::ToolCall { id, .. } if id == "c1"))
            .count(),
        1
    );
    assert!(events.contains(&AgentEvent::ToolCall {
        id: "c1".into(),
        call: ToolCall::Exec {
            command: "ls -la".into()
        },
    }));
    assert!(events.contains(&AgentEvent::ToolResult {
        id: "c1".into(),
        is_error: true,
        output: None,
        diff: None,
    }));

    assert_eq!(
        events
            .iter()
            .filter(|e| matches!(
                e,
                AgentEvent::ToolCall {
                    id,
                    call: ToolCall::WriteFile { path, content: None }
                } if id == "f1" && path == "/tmp/new.rs"
            ))
            .count(),
        2,
        "started + completion-refresh: {events:?}"
    );
    assert!(events.contains(&AgentEvent::ToolResult {
        id: "f1".into(),
        is_error: false,
        output: None,
        diff: None,
    }));

    assert!(events.contains(&AgentEvent::ToolCall {
        id: "mcp1".into(),
        call: ToolCall::Mcp {
            server: "linear".into(),
            tool: "search".into(),
            input: Some(serde_json::json!({"q": "bug"})),
        },
    }));
    assert!(events.contains(&AgentEvent::ToolResult {
        id: "mcp1".into(),
        is_error: true,
        output: None,
        diff: None,
    }));

    assert!(events.contains(&AgentEvent::ToolCall {
        id: "w1".into(),
        call: ToolCall::WebSearch {
            query: "rust".into()
        },
    }));
    assert!(events.contains(&AgentEvent::ToolResult {
        id: "w1".into(),
        is_error: false,
        output: None,
        diff: None,
    }));

    assert!(events.contains(&AgentEvent::ToolCall {
        id: "td1".into(),
        call: ToolCall::Todo {
            items: vec![
                TodoItem::new("a", TodoStatus::Completed),
                TodoItem::new("b", TodoStatus::Pending),
            ]
        },
    }));
    assert!(events.contains(&AgentEvent::ToolResult {
        id: "td1".into(),
        is_error: false,
        output: None,
        diff: None,
    }));

    assert!(
        !events
            .iter()
            .any(|e| matches!(e, AgentEvent::TextDelta { text } if text == "Hello world")),
        "streamed message text re-emitted: {events:?}"
    );
    assert!(events.contains(&AgentEvent::TextDelta {
        text: "unstreamed tail".into()
    }));
    assert_eq!(
        events
            .iter()
            .filter(|e| matches!(e, AgentEvent::AssistantMessageCompleted { .. }))
            .count(),
        2
    );

    let usage_pos = events
        .iter()
        .position(|e| {
            matches!(
                e,
                AgentEvent::Usage {
                    input_tokens: 42,
                    output_tokens: 7
                }
            )
        })
        .expect("usage emitted");
    let done_pos = events
        .iter()
        .position(|e| matches!(e, AgentEvent::Done { .. }))
        .expect("done emitted");
    assert!(usage_pos < done_pos);
    assert_eq!(
        events.last(),
        Some(&AgentEvent::Done {
            status: DoneStatus::Completed,
            result: None,
            error: None,
            session_id: Some("th-1".into()),
        })
    );
}

#[tokio::test]
async fn steering_uses_turn_steer_with_expected_turn_id() {
    let (controls, steer, _token) = controls("Yes");
    steer
        .send(SteerMessage {
            prompt: "redirect please".into(),
            message_id: None,
            attachments: Vec::new(),
            skills: Vec::new(),
        })
        .await
        .expect("steer queued");
    let events = run_to_end(&harness(), request("scenario:steer"), controls).await;

    let steered = events
        .iter()
        .find_map(|e| match e {
            AgentEvent::Steered {
                assistant_message_id,
                next_assistant_message_id,
            } => Some((
                assistant_message_id.clone(),
                next_assistant_message_id.clone(),
            )),
            _ => None,
        })
        .expect("Steered emitted: {events:?}");
    assert!(steered.0.is_some() && steered.1.is_some());
    assert_ne!(steered.0, steered.1);

    // The fake only emits this delta after verifying expectedTurnId and text.
    assert!(events.contains(&AgentEvent::TextDelta {
        text: "steered".into()
    }));
    assert_eq!(
        events.last(),
        Some(&AgentEvent::Done {
            status: DoneStatus::Completed,
            result: None,
            error: None,
            session_id: Some("th-1".into()),
        })
    );
}

#[tokio::test]
async fn rejected_steer_falls_back_to_a_follow_up_turn() {
    let (controls, steer, _token) = controls("Yes");
    steer
        .send(SteerMessage {
            prompt: "redirect please".into(),
            message_id: None,
            attachments: Vec::new(),
            skills: Vec::new(),
        })
        .await
        .expect("steer queued");
    let events = run_to_end(&harness(), request("scenario:steer-race"), controls).await;

    let dones: Vec<_> = events
        .iter()
        .filter_map(|e| match e {
            AgentEvent::Done { status, .. } => Some(*status),
            _ => None,
        })
        .collect();
    assert_eq!(
        dones,
        vec![DoneStatus::Completed, DoneStatus::Completed],
        "{events:?}"
    );
    let steered_pos = events
        .iter()
        .position(|e| matches!(e, AgentEvent::Steered { .. }))
        .expect("Steered emitted on fallback");
    let first_done_pos = events
        .iter()
        .position(|e| matches!(e, AgentEvent::Done { .. }))
        .expect("first done");
    assert!(
        first_done_pos < steered_pos,
        "fallback turn starts after the raced turn ends: {events:?}"
    );
    // The fake only emits this when the fallback turn/start carried the text.
    assert!(events.contains(&AgentEvent::TextDelta {
        text: "fallback".into()
    }));
}

#[tokio::test]
async fn approvals_round_trip_as_input_requests() {
    let asked: Arc<Mutex<Vec<UserInputQuestion>>> = Arc::new(Mutex::new(Vec::new()));
    let (steer_tx, steer_rx) = mpsc::channel(8);
    let _steer = steer_tx;
    let token = CancellationToken::new();
    let seen = asked.clone();
    let controls = RunControls {
        request_input: Box::new(move |questions| {
            seen.lock().unwrap().extend(questions.iter().cloned());
            let (tx, rx) = oneshot::channel();
            let answers: Vec<UserInputAnswer> = questions
                .iter()
                .map(|q| UserInputAnswer {
                    question_id: q.id.clone(),
                    labels: vec!["Allow".into()],
                })
                .collect();
            tx.send(answers).expect("receiver is returned alongside");
            rx
        }),
        steering: steer_rx,
        interrupt: token.clone(),
    };
    let mut req = request("scenario:approve");
    req.permission = PermissionMode::Auto;
    let events = run_to_end(&harness(), req, controls).await;

    let asked = asked.lock().unwrap();
    assert_eq!(asked.len(), 2, "{events:?}");
    assert_eq!(asked[0].header, "Permission");
    assert!(asked[0].question.contains("rm -rf /tmp/x"));
    assert_eq!(
        asked[0].options,
        vec!["Allow".to_string(), "Deny".to_string()]
    );
    assert_eq!(asked[1].header, "Permission");
    assert!(asked[1].question.contains("/tmp/a.rs"));
    assert!(
        !events.iter().any(|e| matches!(
            e,
            AgentEvent::InputRequested { .. } | AgentEvent::InputResolved { .. }
        )),
        "harness must not emit input lifecycle events itself: {events:?}"
    );

    // The fake only completes the turn after seeing both accept decisions.
    assert_eq!(
        events.last(),
        Some(&AgentEvent::Done {
            status: DoneStatus::Completed,
            result: None,
            error: None,
            session_id: Some("th-1".into()),
        })
    );
}

#[tokio::test]
async fn approval_no_answer_becomes_decline() {
    let (controls, _steer, _token) = controls("Deny");
    let mut req = request("scenario:decline");
    req.permission = PermissionMode::Auto;
    let events = run_to_end(&harness(), req, controls).await;

    // The fake only completes the turn after seeing the decline decision.
    assert!(
        matches!(
            events.last(),
            Some(AgentEvent::Done {
                status: DoneStatus::Completed,
                ..
            })
        ),
        "{events:?}"
    );
}

#[tokio::test]
async fn interrupt_sends_turn_interrupt_and_maps_aborted() {
    let (controls, _steer, token) = controls("Yes");
    let mut stream = harness()
        .run(request("scenario:interrupt"), controls)
        .await
        .expect("run starts");

    let events = tokio::time::timeout(Duration::from_secs(10), async move {
        let mut events = Vec::new();
        while let Some(ev) = stream.next().await {
            let ev = ev.expect("stream event");
            if matches!(&ev, AgentEvent::TextDelta { text } if text == "working") {
                token.cancel();
            }
            events.push(ev);
        }
        events
    })
    .await
    .expect("interrupt completed in time");

    assert_eq!(
        events.last(),
        Some(&AgentEvent::Done {
            status: DoneStatus::Interrupted,
            result: None,
            error: None,
            session_id: Some("th-1".into()),
        })
    );
}

#[tokio::test]
async fn unresponsive_child_is_reaped_with_interrupted_done() {
    let harness = CodexHarness::new()
        .with_executable(fixture_path())
        .with_graces(Duration::from_millis(100), Duration::from_millis(500));
    let (controls, _steer, token) = controls("Yes");
    let mut stream = harness
        .run(request("scenario:wedge"), controls)
        .await
        .expect("run starts");

    let events = tokio::time::timeout(Duration::from_secs(10), async move {
        let mut events = Vec::new();
        while let Some(ev) = stream.next().await {
            let ev = ev.expect("stream event");
            if matches!(&ev, AgentEvent::TextDelta { text } if text == "working") {
                token.cancel();
            }
            events.push(ev);
        }
        events
    })
    .await
    .expect("escalation completed in time");

    assert_eq!(
        events.last(),
        Some(&AgentEvent::Done {
            status: DoneStatus::Interrupted,
            result: None,
            error: None,
            session_id: Some("th-1".into()),
        })
    );
}

#[tokio::test]
async fn turn_failed_maps_to_errored_done() {
    let (controls, _steer, _token) = controls("Yes");
    let events = run_to_end(&harness(), request("scenario:fail"), controls).await;
    assert_eq!(
        events.last(),
        Some(&AgentEvent::Done {
            status: DoneStatus::Errored,
            result: None,
            error: Some("boom".into()),
            session_id: Some("th-1".into()),
        })
    );
}

#[tokio::test]
async fn resume_falls_back_to_fresh_thread() {
    let (controls, _steer, _token) = controls("Yes");
    let mut req = request("scenario:resumed");
    req.resume = Some("resume-fail".into());
    let events = run_to_end(&harness(), req, controls).await;

    assert!(
        events.iter().any(|e| matches!(
            e,
            AgentEvent::SessionStarted { session_id, .. } if session_id == "th-fresh"
        )),
        "fresh thread expected: {events:?}"
    );
    assert_eq!(
        events.last(),
        Some(&AgentEvent::Done {
            status: DoneStatus::Completed,
            result: None,
            error: None,
            session_id: Some("th-fresh".into()),
        })
    );
}

#[tokio::test]
async fn resume_reuses_the_existing_thread() {
    let (controls, _steer, _token) = controls("Yes");
    let mut req = request("scenario:resumed");
    req.resume = Some("resume-ok".into());
    let events = run_to_end(&harness(), req, controls).await;

    assert!(
        events.iter().any(|e| matches!(
            e,
            AgentEvent::SessionStarted { session_id, .. } if session_id == "th-resumed"
        )),
        "resumed thread expected: {events:?}"
    );
}

#[tokio::test]
async fn fork_copies_the_thread_into_a_new_one() {
    let (controls, _steer, _token) = controls("Yes");
    let mut req = request("scenario:resumed");
    req.fork = Some("fork-source".into());
    let events = run_to_end(&harness(), req, controls).await;

    assert!(
        events.iter().any(|e| matches!(
            e,
            AgentEvent::SessionStarted { session_id, .. } if session_id == "th-forked"
        )),
        "forked thread expected: {events:?}"
    );
}

#[tokio::test]
async fn missing_binary_is_not_installed() {
    let harness = CodexHarness::new().with_executable("/nonexistent/codex-nowhere");
    let (controls, _steer, _token) = controls("Yes");
    let err = harness
        .run(request("scenario:happy"), controls)
        .await
        .err()
        .expect("spawn fails");
    assert!(matches!(err, HarnessError::NotInstalled(_)), "{err:?}");
}

#[tokio::test]
async fn resumed_parent_recovers_v1_and_v2_child_owners_without_replaying_chips() {
    for mode in ["v1", "v2"] {
        let mut req = request("scenario:resumed-child");
        req.resume = Some(format!("resume-with-child-{mode}"));
        let (controls, _steer, _token) = controls("Yes");
        let events = run_to_end(&harness(), req, controls).await;
        assert!(
            !events.iter().any(
                |e| matches!(e, AgentEvent::ToolCall { call, .. } if call.is_subagent_spawn())
            )
        );
        assert!(events.iter().any(|e| matches!(e,
            AgentEvent::Subagent { parent_tool_use_id, event }
            if parent_tool_use_id == "spawn-alpha" && matches!(event.as_ref(), AgentEvent::TextDelta { text } if text == "resumed alpha")
        )), "{mode}: {events:?}");
    }
}

#[tokio::test]
async fn v2_lifecycle_reuses_chips_and_reopens_the_same_child_for_followup() {
    let (controls, _steer, _token) = controls("Yes");
    let events = run_to_end(&harness(), request("scenario:v2-lifecycle"), controls).await;
    let spawns: Vec<_> = events
        .iter()
        .filter_map(|e| match e {
            AgentEvent::ToolCall { id, call } if call.is_subagent_spawn() => Some(id.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(spawns, ["spawn-alpha", "spawn-beta"]);
    let mut alpha_text = String::new();
    let mut alpha_users = Vec::new();
    let mut alpha_done = Vec::new();
    for e in &events {
        if let AgentEvent::Subagent {
            parent_tool_use_id,
            event,
        } = e
        {
            assert!(spawns.contains(&parent_tool_use_id.as_str()));
            if parent_tool_use_id == "spawn-alpha" {
                match event.as_ref() {
                    AgentEvent::TextDelta { text } => alpha_text.push_str(text),
                    AgentEvent::UserMessage { text } => alpha_users.push(text.as_str()),
                    AgentEvent::Done { status, error, .. } => {
                        alpha_done.push((*status, error.as_deref()))
                    }
                    _ => {}
                }
            }
        }
    }
    assert_eq!(alpha_text, "first alpha\n\nsecond alpha\n\n");
    assert_eq!(alpha_users, ["First assignment"]);
    assert_eq!(
        alpha_done,
        [
            (DoneStatus::Completed, None),
            (DoneStatus::Errored, Some("followup failed"))
        ]
    );
    assert_eq!(
        events
            .iter()
            .filter(|e| matches!(e, AgentEvent::Done { .. }))
            .count(),
        1
    );
}

#[tokio::test]
async fn v1_spawns_bind_children_and_controls_do_not_create_agents() {
    let (controls, _steer, _token) = controls("Yes");
    let events = run_to_end(&harness(), request("scenario:v1-subagents"), controls).await;
    let spawns: std::collections::HashSet<_> = events
        .iter()
        .filter_map(|e| match e {
            AgentEvent::ToolCall { id, call } if call.is_subagent_spawn() => Some(id.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(
        spawns,
        std::collections::HashSet::from(["spawn-alpha", "spawn-beta"])
    );
    for e in &events {
        if let AgentEvent::Subagent {
            parent_tool_use_id, ..
        } = e
        {
            assert!(spawns.contains(parent_tool_use_id.as_str()), "{e:?}");
        }
    }
    for (owner, text) in [
        ("spawn-alpha", "alpha answer"),
        ("spawn-beta", "beta answer"),
    ] {
        assert!(events.iter().any(|e| matches!(e,
            AgentEvent::Subagent { parent_tool_use_id, event }
            if parent_tool_use_id == owner && matches!(event.as_ref(), AgentEvent::TextDelta { text: t } if t == text)
        )));
    }
    assert!(events.iter().any(|e| matches!(e,
        AgentEvent::Subagent { parent_tool_use_id, event }
        if parent_tool_use_id == "spawn-beta" && matches!(event.as_ref(), AgentEvent::ToolCall { id, .. } if id == "beta-tool")
    )));
    assert!(events.iter().any(|e| matches!(e,
        AgentEvent::Subagent { parent_tool_use_id, event }
        if parent_tool_use_id == "spawn-beta" && matches!(event.as_ref(), AgentEvent::UserMessage { text } if text == "Also check gamma")
    )));
    assert_eq!(events.iter().filter(|e| matches!(e, AgentEvent::Subagent { event, .. } if matches!(event.as_ref(), AgentEvent::Done { .. }))).count(), 2);
    let parent: String = events
        .iter()
        .filter_map(|e| match e {
            AgentEvent::TextDelta { text } => Some(text.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(parent, "parent answer");
}

#[tokio::test]
async fn child_identity_survives_early_output_and_later_activity_ids() {
    let (controls, _steer, _token) = controls("Yes");
    let events = run_to_end(&harness(), request("scenario:child-identity"), controls).await;
    let spawn = events
        .iter()
        .position(|e| {
            matches!(e,
                AgentEvent::ToolCall { id, .. } if id == "spawn-alpha"
            )
        })
        .unwrap();
    let early = events.iter().position(|e| matches!(e,
        AgentEvent::Subagent { parent_tool_use_id, event }
        if parent_tool_use_id == "spawn-alpha"
            && matches!(event.as_ref(), AgentEvent::TextDelta { text } if text == "early alpha")
    )).unwrap();
    assert!(
        spawn < early,
        "the chip must exist before buffered traffic binds"
    );
    let mut alpha = String::new();
    let mut beta = String::new();
    for e in &events {
        if let AgentEvent::Subagent {
            parent_tool_use_id,
            event,
        } = e
        {
            assert!(matches!(
                parent_tool_use_id.as_str(),
                "spawn-alpha" | "spawn-beta"
            ));
            if let AgentEvent::TextDelta { text } = event.as_ref() {
                if parent_tool_use_id == "spawn-alpha" {
                    alpha.push_str(text);
                } else {
                    beta.push_str(text);
                }
            }
        }
    }
    assert_eq!(alpha, "early alphalater alpha");
    assert_eq!(beta, "beta outputbeta continues");
    let parent: String = events
        .iter()
        .filter_map(|e| match e {
            AgentEvent::TextDelta { text } => Some(text.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(parent, "parent output");
    assert_eq!(
        events
            .iter()
            .filter(|e| matches!(e, AgentEvent::Done { .. }))
            .count(),
        1
    );
}

#[tokio::test]
async fn child_thread_routing_tags_and_never_settles_parent() {
    let (controls, _steer, _token) = controls("Yes");
    let events = run_to_end(&harness(), request("scenario:subagent"), controls).await;

    let dones: Vec<usize> = events
        .iter()
        .enumerate()
        .filter_map(|(i, e)| matches!(e, AgentEvent::Done { .. }).then_some(i))
        .collect();
    assert_eq!(dones.len(), 1, "one parent Done only: {events:?}");

    let late_parent = events
        .iter()
        .position(|e| matches!(e, AgentEvent::TextDelta { text } if text == "parent still going"))
        .expect("parent delta after child turn end");
    assert!(late_parent < dones[0]);

    assert!(events.iter().any(|e| matches!(
        e,
        AgentEvent::ToolCall { id, call: ToolCall::Unknown { name, .. } }
            if id == "call_alpha" && name == "Agent: alpha"
    )));
    assert!(events.iter().any(
        |e| matches!(e, AgentEvent::ToolResult { id, is_error: false, .. } if id == "call_alpha")
    ));
    assert!(
        !events
            .iter()
            .any(|e| matches!(e, AgentEvent::ToolCall { id, .. } if id == "call_root")),
        "root self-activity must not register or render: {events:?}"
    );

    assert!(events.contains(&AgentEvent::Subagent {
        parent_tool_use_id: "call_alpha".into(),
        event: Box::new(AgentEvent::TextDelta {
            text: "child says hi".into()
        }),
    }));
    assert!(events.contains(&AgentEvent::Subagent {
        parent_tool_use_id: "call_alpha".into(),
        event: Box::new(AgentEvent::ToolCall {
            id: "cs1".into(),
            call: ToolCall::Exec {
                command: "echo hi".into()
            },
        }),
    }));
    assert!(events.contains(&AgentEvent::Subagent {
        parent_tool_use_id: "call_alpha".into(),
        event: Box::new(AgentEvent::ToolResult {
            id: "cs1".into(),
            is_error: false,
            output: None,
            diff: None,
        }),
    }));
    assert_eq!(
        events
            .iter()
            .filter(|e| matches!(
                e,
                AgentEvent::Subagent { parent_tool_use_id, event }
                    if parent_tool_use_id == "call_alpha"
                        && matches!(event.as_ref(), AgentEvent::UserMessage { text } if text == "also check the rebuild")
            ))
            .count(),
        1,
        "{events:?}"
    );
    assert!(
        !events
            .iter()
            .any(|e| matches!(e, AgentEvent::UserMessage { .. })),
        "steer leaked into the parent feed: {events:?}"
    );
    assert!(
        !events
            .iter()
            .any(|e| matches!(e, AgentEvent::ToolCall { id, .. } if id == "cs1")),
        "child tool call leaked into the parent feed: {events:?}"
    );

    assert!(
        events
            .iter()
            .filter(|e| matches!(
                e,
                AgentEvent::Subagent { parent_tool_use_id, event }
                    if parent_tool_use_id == "call_alpha"
                        && matches!(event.as_ref(), AgentEvent::Done { status: DoneStatus::Completed, .. })
            ))
            .count()
            >= 1,
        "{events:?}"
    );
    let child_done = events
        .iter()
        .position(|e| {
            matches!(
                e,
                AgentEvent::Subagent { parent_tool_use_id, event }
                    if parent_tool_use_id == "call_alpha"
                        && matches!(event.as_ref(), AgentEvent::Done { .. })
            )
        })
        .expect("tagged done");
    let late_parent_delta = events
        .iter()
        .position(|e| matches!(e, AgentEvent::TextDelta { text } if text == "parent still going"))
        .expect("parent delta");
    assert!(child_done < late_parent_delta, "{events:?}");
}

#[tokio::test]
#[ignore = "real Codex spawn, followup and resume; needs install, auth and network"]
async fn live_subagent_spawn_and_followup_keep_one_transcript() {
    let executable = std::env::var_os("CODEX_SUBAGENT_TEST_EXECUTABLE")
        .expect("set CODEX_SUBAGENT_TEST_EXECUTABLE to a wrapper selecting v1 or v2");
    let harness = CodexHarness::new().with_executable(executable);
    let cwd = tempfile::tempdir().unwrap();
    let mut req = request(
        "This is an integration smoke test. Spawn EXACTLY ONE subagent (name it alpha if naming is supported). Its entire task is: reply exactly child-first. It must not use any tools or spawn agents. Wait for it to finish, then reply exactly parent-first. Do not close the child; we will reuse it. Do not inspect or change any files.",
    );
    req.cwd = cwd.path().display().to_string();
    req.reasoning = Some(ReasoningLevel::Low);
    if let Ok(model) = std::env::var("CODEX_SUBAGENT_TEST_MODEL") {
        req.model = Some(model);
    }
    let (run_controls, mut steer, mut interrupt) = controls("Yes");
    let mut stream = harness
        .run(req.clone(), run_controls)
        .await
        .expect("run starts");
    let mut events = Vec::new();
    for turn in 0..3 {
        if turn == 1 {
            steer.send(SteerMessage {
                prompt: "Reuse the SAME existing subagent for one more task: reply exactly child-second. Use followup_task if available, otherwise send_input. Do not spawn a new agent. Wait for it to finish, then reply exactly parent-second. Do not inspect or change files.".into(),
                message_id: None,
                attachments: Vec::new(),
                skills: Vec::new(),
            }).await.unwrap();
        }
        if turn == 2 {
            interrupt.cancel();
            tokio::time::timeout(Duration::from_secs(10), async {
                while stream.next().await.is_some() {}
            })
            .await
            .expect("old app-server stops");
            req.resume = events.iter().find_map(|e| match e {
                AgentEvent::SessionStarted { session_id, .. } => Some(session_id.clone()),
                _ => None,
            });
            req.prompt = "The app-server has restarted. Reuse the SAME existing subagent again: reply exactly child-third. Use followup_task if available. Otherwise first use resume_agent with the existing child's id to reactivate it, then send_input. Do not spawn another agent. Wait for its actual child-third reply before replying exactly parent-third. If a tool fails, recover using the same child id; never claim the child finished without its reply. Do not inspect or change files.".into();
            let (run_controls, new_steer, new_interrupt) = controls("Yes");
            steer = new_steer;
            interrupt = new_interrupt;
            stream = harness
                .run(req.clone(), run_controls)
                .await
                .expect("parent resumes");
        }
        let result = tokio::time::timeout(Duration::from_secs(120), async {
            while let Some(event) = stream.next().await {
                let event = event.expect("stream event");
                let done = matches!(event, AgentEvent::Done { .. });
                let failed = matches!(
                    event,
                    AgentEvent::Done {
                        status: DoneStatus::Errored | DoneStatus::Interrupted,
                        ..
                    }
                );
                events.push(event);
                assert!(!failed, "parent failed: {:?}", events.last());
                if done {
                    return;
                }
            }
            panic!("stream ended before parent completion");
        })
        .await;
        if result.is_err() {
            interrupt.cancel();
        }
        result.expect("live turn finishes within 120 seconds");
    }
    drop(stream);
    if let Some(path) = std::env::var_os("CODEX_SUBAGENT_TEST_CAPTURE") {
        std::fs::write(path, serde_json::to_vec_pretty(&events).unwrap()).unwrap();
    }
    let spawns: Vec<_> = events
        .iter()
        .filter_map(|e| match e {
            AgentEvent::ToolCall { id, call } if call.is_subagent_spawn() => Some(id.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(spawns.len(), 1, "one original spawn: {spawns:?}");
    if let Ok(mode) = std::env::var("CODEX_SUBAGENT_TEST_MODE") {
        let expected = match mode.as_str() {
            "v1" => "collabAgentToolCall",
            "v2" => "subAgentActivity",
            _ => panic!("unknown mode {mode}"),
        };
        assert!(
            events.iter().any(|e| matches!(e,
                AgentEvent::ToolCall { call: ToolCall::Unknown { input: Some(input), .. }, .. }
                if input.get("type").and_then(serde_json::Value::as_str) == Some(expected)
            )),
            "the model must actually use {mode}"
        );
    }
    let mut text = String::new();
    let mut terminals = 0;
    for event in &events {
        if let AgentEvent::Subagent {
            parent_tool_use_id,
            event,
        } = event
        {
            assert_eq!(parent_tool_use_id, spawns[0]);
            if let AgentEvent::TextDelta { text: delta } = event.as_ref() {
                text.push_str(delta);
            }
            if matches!(
                event.as_ref(),
                AgentEvent::Done {
                    status: DoneStatus::Completed,
                    ..
                }
            ) {
                terminals += 1;
            }
        }
    }
    for reply in ["child-first", "child-second", "child-third"] {
        assert_eq!(text.matches(reply).count(), 1, "child transcript: {text:?}");
    }
    assert_eq!(terminals, 3, "one child completion per assignment");
}

#[tokio::test]
#[ignore = "spawns the real codex app-server; needs install + auth + network"]
async fn live_real_app_server_single_turn() {
    let harness = CodexHarness::new();
    let mut req = request("Reply with exactly the word: pong");
    req.cwd = std::env::temp_dir().display().to_string();
    let (controls, _steer, _token) = controls("Yes");
    let mut stream = harness.run(req, controls).await.expect("run starts");
    // The session stays open after the turn, so stop at the first Done.
    let events = tokio::time::timeout(Duration::from_secs(120), async {
        let mut events = Vec::new();
        while let Some(ev) = stream.next().await {
            let ev = ev.expect("stream event");
            let done = matches!(ev, AgentEvent::Done { .. });
            events.push(ev);
            if done {
                break;
            }
        }
        events
    })
    .await
    .expect("live turn finished in time");
    assert!(
        events
            .iter()
            .any(|e| matches!(e, AgentEvent::SessionStarted { .. })),
        "{events:?}"
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
async fn image_generation_fake_lifecycle_reaches_done_without_inline_payload() {
    for scenario in ["success", "failure", "missing-path"] {
        let (controls, _steer, _token) = controls("Yes");
        let events = run_to_end(
            &harness(),
            request(&format!("scenario:image-{scenario}")),
            controls,
        )
        .await;
        assert!(events.iter().any(|e| matches!(e, AgentEvent::Done { .. })));
        let results: Vec<_> = events
            .iter()
            .filter(|e| {
                matches!(
                    e,
                    AgentEvent::ToolCall { .. }
                        | AgentEvent::ToolResult { .. }
                        | AgentEvent::GeneratedImage { .. }
                        | AgentEvent::Error { .. }
                )
            })
            .collect();
        assert_eq!(results.len(), 4);
        assert!(matches!(results[0], AgentEvent::ToolCall { .. }));
        assert!(matches!(results[1], AgentEvent::ToolCall { .. }));
        assert!(
            matches!(results[2], AgentEvent::ToolResult { is_error, .. } if *is_error == (scenario != "success"))
        );
        assert_eq!(
            matches!(results[3], AgentEvent::GeneratedImage { .. }),
            scenario == "success"
        );
        assert!(
            !serde_json::to_string(&events)
                .unwrap()
                .contains("INLINE_IMAGE_SENTINEL")
        );
    }
}

#[tokio::test]
#[ignore = "consumes image quota; requires real Codex auth and image generation access"]
async fn real_image_generation_smoke() {
    let (controls, _steer, token) = controls("Yes");
    let mut req = request(
        "Use image generation to create a small green goblin portrait. Generate an image, not text or code.",
    );
    req.model = None;
    req.reasoning = None;
    req.cwd = std::env::temp_dir().display().to_string();
    let mut stream = CodexHarness::new().run(req, controls).await.unwrap();
    let events = tokio::time::timeout(Duration::from_secs(300), async {
        let mut events = Vec::new();
        while let Some(event) = stream.next().await {
            let event = event.unwrap();
            let done = matches!(event, AgentEvent::Done { .. });
            events.push(event);
            if done {
                break;
            }
        }
        events
    })
    .await
    .expect("generation completes in five minutes");
    token.cancel();
    let path = events
        .iter()
        .find_map(|e| {
            if let AgentEvent::GeneratedImage { path, .. } = e {
                Some(path)
            } else {
                None
            }
        })
        .expect("Codex returns savedPath");
    assert!(std::path::Path::new(path).is_absolute());
    assert!(std::path::Path::new(path).is_file());
    assert!(serde_json::to_vec(&events).unwrap().len() < 64 * 1024);
}

#[tokio::test]
async fn native_commands_use_rpc_operations_and_render_results() {
    for (prompt, resume, expected) in [
        ("/compact", true, "Context compacted."),
        ("/review", true, "Review fixture result"),
        (
            "/review check error handling",
            true,
            "Review fixture result",
        ),
    ] {
        let (controls, steer, _) = controls("Yes");
        drop(steer);
        let mut req = request(prompt);
        req.resume = resume.then(|| "existing-thread".into());
        let events = run_to_end(&harness(), req, controls).await;
        let mut text = String::new();
        let mut completions = 0;
        for event in events {
            match event {
                AgentEvent::TextDelta { text: delta } => text.push_str(&delta),
                AgentEvent::Compacted { manual, .. } => {
                    assert!(manual, "{prompt}");
                    text.push_str("Context compacted.");
                }
                AgentEvent::Done { status, error, .. } => {
                    assert_eq!(status, DoneStatus::Completed, "{prompt}: {error:?}");
                    completions += 1;
                }
                _ => {}
            }
        }
        assert_eq!(completions, 1, "{prompt}");
        assert_eq!(text.trim_end(), expected, "{prompt}");
    }
}

#[tokio::test]
async fn native_command_during_a_turn_waits_for_its_boundary() {
    let (controls, steer, _token) = controls("Yes");
    let mut stream = harness()
        .run(request("scenario:native-queue"), controls)
        .await
        .unwrap();
    let mut steer = Some(steer);
    let mut completions = 0;
    let mut output = String::new();
    while let Some(event) = tokio::time::timeout(Duration::from_secs(5), stream.next())
        .await
        .unwrap()
    {
        match event.unwrap() {
            AgentEvent::TextDelta { text } => {
                if text == "working" {
                    steer
                        .take()
                        .unwrap()
                        .send(SteerMessage {
                            prompt: "/review".into(),
                            message_id: None,
                            attachments: Vec::new(),
                            skills: Vec::new(),
                        })
                        .await
                        .unwrap();
                }
                output.push_str(&text);
            }
            AgentEvent::Done { status, error, .. } => {
                assert_eq!(status, DoneStatus::Completed, "{error:?}");
                completions += 1;
            }
            _ => {}
        }
    }
    assert_eq!(completions, 2);
    assert!(output.contains("Queued review result"));
}

#[tokio::test]
async fn ordinary_followup_cannot_overtake_a_queued_native_command() {
    let (controls, steer, _token) = controls("Yes");
    let mut stream = harness()
        .run(request("scenario:native-queue-order"), controls)
        .await
        .unwrap();
    let mut sender = Some(steer);
    let mut events = Vec::new();
    while let Some(event) = tokio::time::timeout(Duration::from_secs(5), stream.next())
        .await
        .unwrap()
    {
        match event.unwrap() {
            AgentEvent::TextDelta { text } => {
                if text == "working" {
                    let sender = sender.take().unwrap();
                    for prompt in ["/review", "Follow up after review"] {
                        sender
                            .send(SteerMessage {
                                prompt: prompt.into(),
                                message_id: None,
                                attachments: Vec::new(),
                                skills: Vec::new(),
                            })
                            .await
                            .unwrap();
                    }
                }
                events.push(text);
            }
            AgentEvent::Steered { .. } => events.push("steered".into()),
            AgentEvent::Done { status, error, .. } => {
                assert_eq!(status, DoneStatus::Completed, "{error:?}");
                events.push("done".into());
            }
            AgentEvent::Error { message } => panic!("{message}"),
            _ => {}
        }
    }
    assert_eq!(
        events,
        [
            "working",
            "done",
            "steered",
            "Queued review result",
            "done",
            "steered",
            "followup",
            "done"
        ]
    );
}

#[tokio::test]
async fn models_discovers_visible_catalog_with_pagination() {
    let models = harness().models().await.expect("models");
    assert_eq!(models.len(), 3);
    assert_eq!(models[0].id, "gpt-6-astra");
    assert_eq!(models[1].id, "gpt-5.6-terra");
    assert_eq!(models[2].id, "gpt-5.6-sol");
    assert!(models[0].reasoning_levels.contains(&ReasoningLevel::Ultra));
    assert!(models.iter().any(|m| m.id == "gpt-5.6-sol"));
    let tier = models[0]
        .options
        .iter()
        .find(|option| option.id == "serviceTier")
        .expect("Astra service tier");
    assert_eq!(tier.choices[0].id, "default");
    assert_eq!(tier.choices[1].id, "fast");
    assert_eq!(tier.choices.len(), 2, "priority and fast dedupe");

    let failed_probe = tempfile::tempdir().unwrap();
    let failed_exe = failed_probe.path().join("failed-codex");
    std::fs::write(&failed_exe, "#!/bin/sh\nexit 1\n").unwrap();
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&failed_exe, std::fs::Permissions::from_mode(0o755)).unwrap();
    let fallback = CodexHarness::new()
        .with_executable(failed_exe)
        .models()
        .await
        .expect("fallback models");
    assert_eq!(fallback.len(), 9);
    assert_eq!(fallback[0].id, "gpt-6-astra");

    let missing = CodexHarness::new().with_executable("/nonexistent/codex-nowhere");
    assert_eq!(missing.id(), HarnessId::Codex);
    assert_eq!(missing.display_name(), "Codex");
    assert_eq!(missing.reasoning_levels().len(), 7);
}

fn skill_ref(name: &str, path: &str) -> SkillRef {
    SkillRef {
        name: name.into(),
        path: path.into(),
    }
}

fn completed_with(events: &[AgentEvent], text: &str) -> bool {
    events
        .iter()
        .any(|event| matches!(event, AgentEvent::TextDelta { text: delta } if delta == text))
        && matches!(
            events.last(),
            Some(AgentEvent::Done {
                status: DoneStatus::Completed,
                ..
            })
        )
}

#[tokio::test]
async fn skills_list_parses_dedupes_and_keeps_disabled_and_pathless() {
    let cwd = tempfile::tempdir().unwrap();
    let skills = harness().skills(cwd.path()).await.expect("skills");
    let skill = |name: &str, description: &str, path: Option<&str>, enabled: bool| Skill {
        name: name.into(),
        description: description.into(),
        path: path.map(str::to_owned),
        enabled,
    };
    assert_eq!(
        skills,
        vec![
            skill(
                "imagegen",
                "Generate or edit images",
                Some("/skills/imagegen/SKILL.md"),
                true
            ),
            skill(
                "bare",
                "No interface block",
                Some("/skills/bare/SKILL.md"),
                true
            ),
            skill("off", "Disabled", Some("/skills/off/SKILL.md"), false),
            skill("pathless", "No path", None, true),
        ]
    );
}

#[tokio::test]
async fn image_attachments_become_local_image_inputs_on_start_and_steer() {
    let (controls, steer, _) = controls("Yes");
    steer
        .send(SteerMessage {
            prompt: "and this".into(),
            message_id: None,
            attachments: vec!["/tmp/steer.png".into()],
            skills: Vec::new(),
        })
        .await
        .unwrap();
    drop(steer);
    let mut req = request("scenario:attachments");
    req.attachments = vec!["/tmp/shot.png".into()];
    let events = run_to_end(&harness(), req, controls).await;
    assert!(
        completed_with(&events, "attachments accepted"),
        "{events:?}"
    );
}

#[tokio::test]
async fn selected_skills_become_skill_inputs_on_start_and_steer() {
    let (controls, steer, _) = controls("Yes");
    steer
        .send(SteerMessage {
            prompt: "Also this".into(),
            message_id: Some("skill-steer".into()),
            attachments: Vec::new(),
            skills: vec![skill_ref("review", "/repo/other/SKILL.md")],
        })
        .await
        .unwrap();
    drop(steer);
    let mut req = request("scenario:native-skills");
    req.skills = vec![skill_ref("review", "/repo/a b/SKILL.md")];
    let events = run_to_end(&harness(), req, controls).await;
    assert!(
        completed_with(&events, "native skills accepted"),
        "{events:?}"
    );
}

#[tokio::test]
async fn skills_survive_a_rejected_steer_falling_back_to_a_new_turn() {
    let (controls, steer, _) = controls("Yes");
    steer
        .send(SteerMessage {
            prompt: "redirect please".into(),
            message_id: None,
            attachments: Vec::new(),
            skills: vec![skill_ref("followup", "/repo/followup/SKILL.md")],
        })
        .await
        .unwrap();
    let events = run_to_end(&harness(), request("scenario:steer-race"), controls).await;
    assert!(completed_with(&events, "fallback"), "{events:?}");
}

#[tokio::test]
async fn commands_reject_attachments_and_skills() {
    let (ctl, _, _) = controls("Yes");
    let mut req = request("/review");
    req.attachments.push("/tmp/image.png".into());
    assert!(harness().run(req, ctl).await.is_err());

    let (ctl, _, _) = controls("Yes");
    let mut req = request("/review");
    req.skills.push(skill_ref("review", "/repo/SKILL.md"));
    assert!(harness().run(req, ctl).await.is_err());
}
