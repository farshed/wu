use std::sync::Arc;

use anyhow::{Context as _, Result};
use futures::{StreamExt, stream::BoxStream};
use gpui::TestAppContext;
use parking_lot::Mutex;
use rpc::{ConnectionId, Peer, Receipt, TypedEnvelope, proto};

use crate::{Client, Connection};

pub struct FakeServer {
    peer: Arc<Peer>,
    state: Arc<Mutex<FakeServerState>>,
}

#[derive(Default)]
struct FakeServerState {
    incoming: Option<BoxStream<'static, Box<dyn proto::AnyTypedEnvelope>>>,
    connection_id: Option<ConnectionId>,
}

impl FakeServer {
    pub async fn for_client(client: &Arc<Client>, cx: &TestAppContext) -> Self {
        let server = Self {
            peer: Peer::new(0),
            state: Default::default(),
        };

        let cx = cx.to_async();
        let (client_conn, server_conn, _) = Connection::in_memory(cx.background_executor().clone());
        let (connection_id, io, incoming) = server
            .peer
            .add_test_connection(server_conn, cx.background_executor().clone());
        cx.background_executor().spawn(io).detach();
        {
            let mut state = server.state.lock();
            state.connection_id = Some(connection_id);
            state.incoming = Some(incoming);
        }
        server
            .peer
            .send(
                connection_id,
                proto::Hello {
                    peer_id: Some(connection_id.into()),
                },
            )
            .unwrap();

        client.set_connection(client_conn, &cx).await.unwrap();

        server
    }

    pub fn disconnect(&self) {
        if self.state.lock().connection_id.is_some() {
            self.peer.disconnect(self.connection_id());
            let mut state = self.state.lock();
            state.connection_id.take();
            state.incoming.take();
        }
    }

    pub fn send<T: proto::EnvelopedMessage>(&self, message: T) {
        self.peer.send(self.connection_id(), message).unwrap();
    }

    #[allow(clippy::await_holding_lock)]
    pub async fn receive<M: proto::EnvelopedMessage>(&self) -> Result<TypedEnvelope<M>> {
        let message = self
            .state
            .lock()
            .incoming
            .as_mut()
            .expect("not connected")
            .next()
            .await
            .context("other half hung up")?;
        let type_name = message.payload_type_name();
        let message = message.into_any();

        if message.is::<TypedEnvelope<M>>() {
            return Ok(*message.downcast().unwrap());
        }

        panic!(
            "fake server received unexpected message type: {:?}",
            type_name
        );
    }

    pub fn respond<T: proto::RequestMessage>(&self, receipt: Receipt<T>, response: T::Response) {
        self.peer.respond(receipt, response).unwrap()
    }

    fn connection_id(&self) -> ConnectionId {
        self.state.lock().connection_id.expect("not connected")
    }
}

impl Drop for FakeServer {
    fn drop(&mut self) {
        self.disconnect();
    }
}
