use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};
use std::sync::{Arc, Mutex, PoisonError};

use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::sync::{mpsc, oneshot};

use crate::HarnessError;
use crate::process::{ChildStdin, ChildStdout};

#[derive(Debug)]
pub(crate) enum Incoming {
    Notification {
        method: String,
        params: Value,
    },
    /// Must be answered via [`RpcClient::respond`] or [`RpcClient::respond_error`].
    Request {
        id: Value,
        method: String,
        params: Value,
    },
    Eof,
}

type StdoutObserver = Box<dyn Fn(&str) + Send>;

type Pending = Arc<Mutex<HashMap<i64, oneshot::Sender<Result<Value, String>>>>>;

#[derive(Clone)]
pub(crate) struct RpcClient {
    next_id: Arc<AtomicI64>,
    pending: Pending,
    writer: mpsc::UnboundedSender<String>,
    closed: Arc<AtomicBool>,
}

impl RpcClient {
    pub fn new(stdin: ChildStdin, stdout: ChildStdout) -> (Self, mpsc::Receiver<Incoming>) {
        Self::with_stdout_observer(stdin, stdout, None)
    }

    pub(crate) fn with_stdout_observer(
        stdin: ChildStdin,
        stdout: ChildStdout,
        observer: Option<StdoutObserver>,
    ) -> (Self, mpsc::Receiver<Incoming>) {
        let (writer_tx, writer_rx) = mpsc::unbounded_channel::<String>();
        tokio::spawn(write_loop(stdin, writer_rx));
        let pending: Pending = Arc::default();
        let (incoming_tx, incoming_rx) = mpsc::channel(256);
        let closed = Arc::new(AtomicBool::new(false));
        tokio::spawn(read_loop(
            stdout,
            Arc::clone(&pending),
            incoming_tx,
            closed.clone(),
            observer,
        ));
        (
            Self {
                next_id: Arc::new(AtomicI64::new(0)),
                pending,
                writer: writer_tx,
                closed,
            },
            incoming_rx,
        )
    }

    pub fn is_closed(&self) -> bool {
        self.closed.load(Ordering::Acquire)
    }

    pub async fn request(&self, method: &str, params: Value) -> Result<Value, HarnessError> {
        self.request_now(method, params).await
    }

    /// Queues the line before returning, so a later notification cannot overtake it.
    pub fn request_now(
        &self,
        method: &str,
        params: Value,
    ) -> futures::future::BoxFuture<'static, Result<Value, HarnessError>> {
        let method = method.to_owned();
        let id = self.next_id.fetch_add(1, Ordering::Relaxed) + 1;
        let (tx, rx) = oneshot::channel();
        {
            let mut pending = self.pending.lock().unwrap_or_else(PoisonError::into_inner);
            // Checked under the EOF-cleanup lock so a racing request is rejected or cleared.
            if self.is_closed() {
                return Box::pin(async move {
                    Err(HarnessError::Protocol(format!(
                        "{method}: app-server exited before responding"
                    )))
                });
            }
            pending.insert(id, tx);
        }
        let line = json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params });
        if self.writer.send(line.to_string()).is_err() {
            self.pending
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .remove(&id);
            return Box::pin(async move {
                Err(HarnessError::Protocol(format!(
                    "{method}: app-server stdin closed"
                )))
            });
        }
        Box::pin(async move {
            match rx.await {
                Ok(Ok(result)) => Ok(result),
                Ok(Err(message)) => Err(HarnessError::Protocol(format!("{method}: {message}"))),
                Err(_) => Err(HarnessError::Protocol(format!(
                    "{method}: app-server exited before responding"
                ))),
            }
        })
    }

    pub fn notify(&self, method: &str, params: Option<Value>) {
        let line = match params {
            Some(params) => json!({ "jsonrpc": "2.0", "method": method, "params": params }),
            None => json!({ "jsonrpc": "2.0", "method": method }),
        };
        self.write_line(line);
    }

    pub fn respond(&self, id: &Value, result: Value) {
        let line = json!({ "jsonrpc": "2.0", "id": id, "result": result });
        self.write_line(line);
    }

    pub fn respond_error(&self, id: &Value, code: i64, message: &str) {
        let line = json!({
            "jsonrpc": "2.0",
            "id": id,
            "error": { "code": code, "message": message },
        });
        self.write_line(line);
    }

    fn write_line(&self, line: Value) {
        if self.writer.send(line.to_string()).is_err() {
            tracing::debug!(target: "agent_harness::rpc", "stdin writer gone, dropping frame");
        }
    }
}

async fn write_loop(mut stdin: ChildStdin, mut rx: mpsc::UnboundedReceiver<String>) {
    while let Some(line) = rx.recv().await {
        let write = async {
            stdin.write_all(line.as_bytes()).await?;
            stdin.write_all(b"\n").await?;
            stdin.flush().await
        };
        if let Err(e) = write.await {
            tracing::debug!(target: "agent_harness::rpc", "stdin write failed (tolerated): {e}");
            return;
        }
    }
}

/// Servers may echo our numeric ids re-encoded as `"5"` or `5.0`; dropping those strands the caller.
fn response_id(id: &Value) -> Option<i64> {
    if let Some(n) = id.as_i64() {
        return Some(n);
    }
    if let Some(s) = id.as_str() {
        return s.parse().ok();
    }
    id.as_f64().filter(|f| f.fract() == 0.0).map(|f| f as i64)
}

fn response_error(error: &Value) -> String {
    let Some(message) = error.get("message").and_then(Value::as_str) else {
        return error.to_string();
    };
    let mut rendered = message.to_owned();
    if let Some(code) = error.get("code").and_then(Value::as_i64) {
        rendered.push_str(&format!(" (code {code})"));
    }
    if let Some(data) = error.get("data").filter(|v| !v.is_null()) {
        let detail = data
            .as_str()
            .map(str::to_owned)
            .unwrap_or_else(|| data.to_string());
        if !detail.is_empty() {
            rendered.push_str(": ");
            rendered.push_str(&detail);
        }
    }
    rendered
}

async fn read_loop(
    stdout: ChildStdout,
    pending: Pending,
    tx: mpsc::Sender<Incoming>,
    closed: Arc<AtomicBool>,
    observer: Option<StdoutObserver>,
) {
    let mut lines = BufReader::new(stdout).lines();
    while let Ok(Some(line)) = lines.next_line().await {
        let line = line.trim();
        if let Some(url) =
            line.strip_prefix("Open the following link to authenticate the ACP server: ")
        {
            if let Some(observer) = &observer {
                observer(url);
            }
            continue;
        }
        if line.is_empty() {
            continue;
        }
        let Ok(mut msg) = serde_json::from_str::<Value>(line) else {
            tracing::debug!(target: "agent_harness::rpc", "non-JSON stdout line (skipped)");
            continue;
        };
        if !msg.is_object() || msg.get("jsonrpc").is_some_and(|version| version != "2.0") {
            continue;
        }
        let method = msg.get("method").and_then(Value::as_str);
        let id = msg.get("id");
        match (method, id) {
            (None, Some(id)) => {
                if msg.get("result").is_none() && msg.get("error").is_none() {
                    continue;
                }
                let Some(id) = response_id(id) else { continue };
                let Some(sender) = pending
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .remove(&id)
                else {
                    continue;
                };
                let outcome = match msg.get("error") {
                    Some(err) => Err(response_error(err)),
                    None => Ok(msg
                        .get_mut("result")
                        .map(Value::take)
                        .unwrap_or(Value::Null)),
                };
                if sender.send(outcome).is_err() {
                    tracing::debug!(target: "agent_harness::rpc", "response {id} arrived after its caller gave up");
                }
            }
            (Some(method), Some(id)) => {
                let incoming = Incoming::Request {
                    id: id.clone(),
                    method: method.to_owned(),
                    params: msg
                        .get_mut("params")
                        .map(Value::take)
                        .unwrap_or(Value::Null),
                };
                if tx.send(incoming).await.is_err() {
                    return;
                }
            }
            (Some(method), None) => {
                let incoming = Incoming::Notification {
                    method: method.to_owned(),
                    params: msg
                        .get_mut("params")
                        .map(Value::take)
                        .unwrap_or(Value::Null),
                };
                if tx.send(incoming).await.is_err() {
                    return;
                }
            }
            (None, None) => {}
        }
    }
    closed.store(true, Ordering::Release);
    pending
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .clear();
    if tx.send(Incoming::Eof).await.is_err() {
        tracing::debug!(target: "agent_harness::rpc", "incoming receiver gone before EOF");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cancel_notification_wire_has_no_id() {
        let (writer, mut receiver) = mpsc::unbounded_channel();
        let client = RpcClient {
            next_id: Arc::new(AtomicI64::new(0)),
            pending: Arc::default(),
            writer,
            closed: Arc::new(AtomicBool::new(false)),
        };
        client.notify("session/cancel", Some(json!({"sessionId": "parent"})));
        let frame: Value = serde_json::from_str(&receiver.try_recv().unwrap()).unwrap();
        assert_eq!(
            frame,
            json!({"jsonrpc": "2.0", "method": "session/cancel", "params": {"sessionId": "parent"}})
        );
        assert!(frame.get("id").is_none());
        assert!(client.pending.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn requests_after_eof_fail_without_entering_pending_map() {
        let (writer, mut receiver) = mpsc::unbounded_channel();
        let client = RpcClient {
            next_id: Arc::new(AtomicI64::new(0)),
            pending: Arc::default(),
            writer,
            closed: Arc::new(AtomicBool::new(true)),
        };
        let result = tokio::time::timeout(
            std::time::Duration::from_millis(100),
            client.request("session/prompt", json!({})),
        )
        .await
        .expect("a request after EOF cannot wait for another EOF");
        assert!(result.unwrap_err().to_string().contains("exited"));
        assert!(client.pending.lock().unwrap().is_empty());
        assert!(receiver.try_recv().is_err());
    }

    #[test]
    fn rpc_error_preserves_string_and_structured_details() {
        for data in [
            json!("A prompt is already running"),
            json!({"reason": "A prompt is already running"}),
            json!(["busy", 7]),
        ] {
            let rendered = response_error(
                &json!({"code": -32600, "message": "Invalid request", "data": data}),
            );
            assert!(rendered.starts_with("Invalid request (code -32600): "));
            assert!(rendered.ends_with(data.as_str().unwrap_or(&data.to_string())));
        }
    }

    #[test]
    fn rpc_error_handles_missing_null_and_unstructured_fields() {
        for error in [
            json!({"message": "busy"}),
            json!({"message": "busy", "data": null}),
            json!({"message": "busy", "data": ""}),
        ] {
            assert_eq!(response_error(&error), "busy");
        }
        for error in [
            json!({"code": -32600, "data": {"reason": "busy"}}),
            json!("unstructured error"),
        ] {
            assert_eq!(response_error(&error), error.to_string());
        }
    }
}
