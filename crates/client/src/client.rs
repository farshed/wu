#[cfg(any(test, feature = "test-support"))]
pub mod test;

pub mod os_info;
pub mod user;
pub mod zed_urls;

use anyhow::Result;
use futures::{FutureExt, Stream, TryFutureExt as _, future::BoxFuture, stream::BoxStream};
use gpui::{App, AsyncApp, Entity, Global, WeakEntity};
use http_client::{HttpClientWithUrl, read_proxy_from_env};
use parking_lot::{Mutex, RwLock};
use postage::watch;
use rpc::proto::{AnyTypedEnvelope, EnvelopedMessage, PeerId, RequestMessage};
use serde::Deserialize;
use settings::{RegisterSetting, Settings};
use std::{
    any::TypeId,
    future::Future,
    marker::PhantomData,
    sync::{
        Arc, LazyLock, Weak,
        atomic::{AtomicU64, Ordering},
    },
};
use url::Url;
use util::ResultExt;

pub use rpc::*;
pub use user::*;

static ZED_SERVER_URL: LazyLock<Option<String>> =
    LazyLock::new(|| std::env::var("ZED_SERVER_URL").ok());

#[derive(Deserialize, RegisterSetting)]
pub struct ClientSettings {
    pub server_url: String,
    /// Overrides the key used to store credentials in the system keychain.
    /// Defaults to `server_url` when unset.
    ///
    /// Useful when running multiple Zed instances side by side without them
    /// overwriting each other's keychain entries.
    ///
    /// Note: changing this after signing in will require signing in again, as
    /// existing credentials are stored under the old key.
    pub credentials_url: Option<String>,
}

impl Settings for ClientSettings {
    fn from_settings(content: &settings::SettingsContent) -> Self {
        if let Some(server_url) = &*ZED_SERVER_URL {
            return Self {
                server_url: server_url.clone(),
                credentials_url: content.credentials_url.clone(),
            };
        }
        Self {
            server_url: content.server_url.clone().unwrap(),
            credentials_url: content.credentials_url.clone(),
        }
    }
}

#[derive(Deserialize, Default, RegisterSetting)]
pub struct ProxySettings {
    pub proxy: Option<String>,
}

impl ProxySettings {
    pub fn proxy_url(&self) -> Option<Url> {
        self.proxy
            .as_deref()
            .map(str::trim)
            .filter(|input| !input.is_empty())
            .and_then(|input| {
                input
                    .parse::<Url>()
                    .inspect_err(|e| log::error!("Error parsing proxy settings: {}", e))
                    .ok()
            })
            .or_else(read_proxy_from_env)
    }
}

impl Settings for ProxySettings {
    fn from_settings(content: &settings::SettingsContent) -> Self {
        Self {
            proxy: content
                .proxy
                .as_deref()
                .map(str::trim)
                .filter(|proxy| !proxy.is_empty())
                .map(ToOwned::to_owned),
        }
    }
}
struct GlobalClient(Arc<Client>);

impl Global for GlobalClient {}

pub struct Client {
    id: AtomicU64,
    peer: Arc<Peer>,
    http: Arc<HttpClientWithUrl>,
    state: RwLock<ClientState>,
    handler_set: Mutex<ProtoMessageHandlerSet>,
}

#[derive(Copy, Clone, Debug, PartialEq)]
pub enum Status {
    SignedOut,
    Connected {
        peer_id: PeerId,
        connection_id: ConnectionId,
    },
    ConnectionLost,
}

impl Status {
    pub fn is_connected(&self) -> bool {
        matches!(self, Self::Connected { .. })
    }
}

struct ClientState {
    status: (watch::Sender<Status>, watch::Receiver<Status>),
}

impl Default for ClientState {
    fn default() -> Self {
        Self {
            status: watch::channel_with(Status::SignedOut),
        }
    }
}

pub enum Subscription {
    Entity {
        client: Weak<Client>,
        id: (TypeId, u64),
    },
    Message {
        client: Weak<Client>,
        id: TypeId,
    },
}

impl Drop for Subscription {
    fn drop(&mut self) {
        match self {
            Subscription::Entity { client, id } => {
                if let Some(client) = client.upgrade() {
                    let mut state = client.handler_set.lock();
                    let _ = state.entities_by_type_and_remote_id.remove(id);
                }
            }
            Subscription::Message { client, id } => {
                if let Some(client) = client.upgrade() {
                    let mut state = client.handler_set.lock();
                    let _ = state.entity_types_by_message_type.remove(id);
                    let _ = state.message_handlers.remove(id);
                }
            }
        }
    }
}

pub struct PendingEntitySubscription<T: 'static> {
    client: Arc<Client>,
    remote_id: u64,
    _entity_type: PhantomData<T>,
    consumed: bool,
}

impl<T: 'static> PendingEntitySubscription<T> {
    pub fn set_entity(mut self, entity: &Entity<T>, cx: &AsyncApp) -> Subscription {
        self.consumed = true;
        let mut handlers = self.client.handler_set.lock();
        let id = (TypeId::of::<T>(), self.remote_id);
        let Some(EntityMessageSubscriber::Pending(messages)) =
            handlers.entities_by_type_and_remote_id.remove(&id)
        else {
            unreachable!()
        };

        handlers.entities_by_type_and_remote_id.insert(
            id,
            EntityMessageSubscriber::Entity {
                handle: entity.downgrade().into(),
            },
        );
        drop(handlers);
        for message in messages {
            let client_id = self.client.id();
            let type_name = message.payload_type_name();
            let sender_id = message.original_sender_id();
            log::debug!(
                "handling queued rpc message. client_id:{}, sender_id:{:?}, type:{}",
                client_id,
                sender_id,
                type_name
            );
            self.client.handle_message(message, cx);
        }
        Subscription::Entity {
            client: Arc::downgrade(&self.client),
            id,
        }
    }
}

impl<T: 'static> Drop for PendingEntitySubscription<T> {
    fn drop(&mut self) {
        if !self.consumed {
            let mut state = self.client.handler_set.lock();
            if let Some(EntityMessageSubscriber::Pending(messages)) = state
                .entities_by_type_and_remote_id
                .remove(&(TypeId::of::<T>(), self.remote_id))
            {
                for message in messages {
                    log::info!("unhandled message {}", message.payload_type_name());
                }
            }
        }
    }
}

impl Client {
    pub fn new(http: Arc<HttpClientWithUrl>) -> Arc<Self> {
        Arc::new(Self {
            id: AtomicU64::new(0),
            peer: Peer::new(0),
            http,
            state: Default::default(),
            handler_set: Default::default(),
        })
    }

    pub fn production(cx: &mut App) -> Arc<Self> {
        let http = Arc::new(HttpClientWithUrl::new_url(
            cx.http_client(),
            &ClientSettings::get_global(cx).server_url,
            cx.http_client().proxy().cloned(),
        ));
        Self::new(http)
    }

    pub fn id(&self) -> u64 {
        self.id.load(Ordering::SeqCst)
    }

    pub fn http_client(&self) -> Arc<HttpClientWithUrl> {
        self.http.clone()
    }

    pub fn set_id(&self, id: u64) -> &Self {
        self.id.store(id, Ordering::SeqCst);
        self
    }

    #[cfg(any(test, feature = "test-support"))]
    pub fn teardown(&self) {
        self.handler_set.lock().clear();
        self.peer.teardown();
    }

    pub fn global(cx: &App) -> Arc<Self> {
        cx.global::<GlobalClient>().0.clone()
    }
    pub fn set_global(client: Arc<Client>, cx: &mut App) {
        cx.set_global(GlobalClient(client))
    }

    pub fn peer_id(&self) -> Option<PeerId> {
        if let Status::Connected { peer_id, .. } = &*self.status().borrow() {
            Some(*peer_id)
        } else {
            None
        }
    }

    pub fn status(&self) -> watch::Receiver<Status> {
        self.state.read().status.1.clone()
    }

    #[cfg(any(test, feature = "test-support"))]
    fn set_status(&self, status: Status) {
        log::info!("set status on client {}: {:?}", self.id(), status);
        let mut state = self.state.write();
        *state.status.0.borrow_mut() = status;
    }

    pub fn subscribe_to_entity<T>(
        self: &Arc<Self>,
        remote_id: u64,
    ) -> Result<PendingEntitySubscription<T>>
    where
        T: 'static,
    {
        let id = (TypeId::of::<T>(), remote_id);

        let mut state = self.handler_set.lock();
        anyhow::ensure!(
            !state.entities_by_type_and_remote_id.contains_key(&id),
            "already subscribed to entity"
        );

        state
            .entities_by_type_and_remote_id
            .insert(id, EntityMessageSubscriber::Pending(Default::default()));

        Ok(PendingEntitySubscription {
            client: self.clone(),
            remote_id,
            consumed: false,
            _entity_type: PhantomData,
        })
    }

    #[track_caller]
    pub fn add_message_handler<M, E, H, F>(
        self: &Arc<Self>,
        entity: WeakEntity<E>,
        handler: H,
    ) -> Subscription
    where
        M: EnvelopedMessage,
        E: 'static,
        H: 'static + Sync + Fn(Entity<E>, TypedEnvelope<M>, AsyncApp) -> F + Send + Sync,
        F: 'static + Future<Output = Result<()>>,
    {
        self.add_message_handler_impl(entity, move |entity, message, _, cx| {
            handler(entity, message, cx)
        })
    }

    fn add_message_handler_impl<M, E, H, F>(
        self: &Arc<Self>,
        entity: WeakEntity<E>,
        handler: H,
    ) -> Subscription
    where
        M: EnvelopedMessage,
        E: 'static,
        H: 'static
            + Sync
            + Fn(Entity<E>, TypedEnvelope<M>, AnyProtoClient, AsyncApp) -> F
            + Send
            + Sync,
        F: 'static + Future<Output = Result<()>>,
    {
        let message_type_id = TypeId::of::<M>();
        let mut state = self.handler_set.lock();
        state
            .entities_by_message_type
            .insert(message_type_id, entity.into());

        let prev_handler = state.message_handlers.insert(
            message_type_id,
            Arc::new(move |subscriber, envelope, client, cx| {
                let subscriber = subscriber.downcast::<E>().unwrap();
                let envelope = envelope.into_any().downcast::<TypedEnvelope<M>>().unwrap();
                handler(subscriber, *envelope, client, cx).boxed_local()
            }),
        );
        if prev_handler.is_some() {
            let location = std::panic::Location::caller();
            panic!(
                "{}:{} registered handler for the same message {} twice",
                location.file(),
                location.line(),
                std::any::type_name::<M>()
            );
        }

        Subscription::Message {
            client: Arc::downgrade(self),
            id: message_type_id,
        }
    }

    pub fn add_request_handler<M, E, H, F>(
        self: &Arc<Self>,
        entity: WeakEntity<E>,
        handler: H,
    ) -> Subscription
    where
        M: RequestMessage,
        E: 'static,
        H: 'static + Sync + Fn(Entity<E>, TypedEnvelope<M>, AsyncApp) -> F + Send + Sync,
        F: 'static + Future<Output = Result<M::Response>>,
    {
        self.add_message_handler_impl(entity, move |handle, envelope, this, cx| {
            Self::respond_to_request(envelope.receipt(), handler(handle, envelope, cx), this)
        })
    }

    async fn respond_to_request<T: RequestMessage, F: Future<Output = Result<T::Response>>>(
        receipt: Receipt<T>,
        response: F,
        client: AnyProtoClient,
    ) -> Result<()> {
        match response.await {
            Ok(response) => {
                client.send_response(receipt.message_id, response)?;
                Ok(())
            }
            Err(error) => {
                client.send_response(receipt.message_id, error.to_proto())?;
                Err(error)
            }
        }
    }

    #[cfg(any(test, feature = "test-support"))]
    pub async fn set_connection(self: &Arc<Self>, conn: Connection, cx: &AsyncApp) -> Result<()> {
        use anyhow::{Context as _, anyhow};
        use futures::StreamExt as _;

        let executor = cx.background_executor();
        log::debug!("add connection to peer");
        let (connection_id, handle_io, mut incoming) = self.peer.add_connection(conn, {
            let executor = executor.clone();
            move |duration| executor.timer(duration)
        });
        let handle_io = executor.spawn(handle_io);

        let peer_id = async {
            log::debug!("waiting for server hello");
            let message = incoming.next().await.context("no hello message received")?;
            log::debug!("got server hello");
            let hello_message_type_name = message.payload_type_name().to_string();
            let hello = message
                .into_any()
                .downcast::<TypedEnvelope<proto::Hello>>()
                .map_err(|_| {
                    anyhow!(
                        "invalid hello message received: {:?}",
                        hello_message_type_name
                    )
                })?;
            let peer_id = hello.payload.peer_id.context("invalid peer id")?;
            Ok(peer_id)
        };

        let peer_id = match peer_id.await {
            Ok(peer_id) => peer_id,
            Err(error) => {
                self.peer.disconnect(connection_id);
                return Err(error);
            }
        };

        log::debug!(
            "set status to connected (connection id: {:?}, peer id: {:?})",
            connection_id,
            peer_id
        );
        self.set_status(Status::Connected {
            peer_id,
            connection_id,
        });

        cx.spawn({
            let this = self.clone();
            async move |cx| {
                while let Some(message) = incoming.next().await {
                    this.handle_message(message, cx);
                    // Don't starve the main thread when receiving lots of messages at once.
                    smol::future::yield_now().await;
                }
            }
        })
        .detach();

        cx.spawn({
            let this = self.clone();
            async move |_| match handle_io.await {
                Ok(()) => {
                    if *this.status().borrow()
                        == (Status::Connected {
                            connection_id,
                            peer_id,
                        })
                    {
                        this.set_status(Status::SignedOut);
                    }
                }
                Err(err) => {
                    log::error!("connection error: {:?}", err);
                    this.set_status(Status::ConnectionLost);
                }
            }
        })
        .detach();

        Ok(())
    }
    fn connection_id(&self) -> Result<ConnectionId> {
        if let Status::Connected { connection_id, .. } = *self.status().borrow() {
            Ok(connection_id)
        } else {
            anyhow::bail!("not connected");
        }
    }

    pub fn send<T: EnvelopedMessage>(&self, message: T) -> Result<()> {
        log::debug!("rpc send. client_id:{}, name:{}", self.id(), T::NAME);
        self.peer.send(self.connection_id()?, message)
    }

    pub fn request<T: RequestMessage>(
        &self,
        request: T,
    ) -> impl Future<Output = Result<T::Response>> + use<T> {
        self.request_envelope(request)
            .map_ok(|envelope| envelope.payload)
    }

    pub fn request_stream<T: RequestMessage>(
        &self,
        request: T,
    ) -> impl Future<Output = Result<impl Stream<Item = Result<T::Response>>>> {
        let client_id = self.id.load(Ordering::SeqCst);
        log::debug!(
            "rpc request start. client_id:{}. name:{}",
            client_id,
            T::NAME
        );
        let response = self
            .connection_id()
            .map(|conn_id| self.peer.request_stream(conn_id, request));
        async move {
            let response = response?.await;
            log::debug!(
                "rpc request finish. client_id:{}. name:{}",
                client_id,
                T::NAME
            );
            response
        }
    }

    pub fn request_envelope<T: RequestMessage>(
        &self,
        request: T,
    ) -> impl Future<Output = Result<TypedEnvelope<T::Response>>> + use<T> {
        let client_id = self.id();
        log::debug!(
            "rpc request start. client_id:{}. name:{}",
            client_id,
            T::NAME
        );
        let response = self
            .connection_id()
            .map(|conn_id| self.peer.request_envelope(conn_id, request));
        async move {
            let response = response?.await;
            log::debug!(
                "rpc request finish. client_id:{}. name:{}",
                client_id,
                T::NAME
            );
            response
        }
    }

    pub fn request_dynamic(
        &self,
        envelope: proto::Envelope,
        request_type: &'static str,
    ) -> impl Future<Output = Result<proto::Envelope>> + use<> {
        let client_id = self.id();
        log::debug!(
            "rpc request start. client_id:{}. name:{}",
            client_id,
            request_type
        );
        let response = self
            .connection_id()
            .map(|conn_id| self.peer.request_dynamic(conn_id, envelope, request_type));
        async move {
            let response = response?.await;
            log::debug!(
                "rpc request finish. client_id:{}. name:{}",
                client_id,
                request_type
            );
            Ok(response?.0)
        }
    }

    fn handle_message(self: &Arc<Client>, message: Box<dyn AnyTypedEnvelope>, cx: &AsyncApp) {
        let sender_id = message.sender_id();
        let request_id = message.message_id();
        let type_name = message.payload_type_name();
        let original_sender_id = message.original_sender_id();

        if let Some(future) = ProtoMessageHandlerSet::handle_message(
            &self.handler_set,
            message,
            self.clone().into(),
            cx.clone(),
        ) {
            let client_id = self.id();
            log::debug!(
                "rpc message received. client_id:{}, sender_id:{:?}, type:{}",
                client_id,
                original_sender_id,
                type_name
            );
            cx.spawn(async move |_| match future.await {
                Ok(()) => {
                    log::debug!("rpc message handled. client_id:{client_id}, sender_id:{original_sender_id:?}, type:{type_name}");
                }
                Err(error) => {
                    log::error!("error handling message. client_id:{client_id}, sender_id:{original_sender_id:?}, type:{type_name}, error:{error:#}");
                }
            })
            .detach();
        } else {
            log::info!("unhandled message {}", type_name);
            self.peer
                .respond_with_unhandled_message(sender_id.into(), request_id, type_name)
                .log_err();
        }
    }
}

impl ProtoClient for Client {
    fn request(
        &self,
        envelope: proto::Envelope,
        request_type: &'static str,
    ) -> BoxFuture<'static, Result<proto::Envelope>> {
        self.request_dynamic(envelope, request_type).boxed()
    }

    fn request_stream(
        &self,
        envelope: proto::Envelope,
        request_type: &'static str,
    ) -> BoxFuture<'static, Result<BoxStream<'static, Result<proto::Envelope>>>> {
        let client_id = self.id();
        let response = self.connection_id().map(|connection_id| {
            self.peer
                .request_stream_dynamic(connection_id, envelope, request_type)
        });

        async move {
            log::debug!(
                "rpc stream request start. client_id:{}. name:{}",
                client_id,
                request_type
            );
            let response = response?.await;
            log::debug!(
                "rpc stream request opened. client_id:{}. name:{}",
                client_id,
                request_type
            );
            response
        }
        .boxed()
    }

    fn send(&self, envelope: proto::Envelope, message_type: &'static str) -> Result<()> {
        log::debug!("rpc send. client_id:{}, name:{}", self.id(), message_type);
        let connection_id = self.connection_id()?;
        self.peer.send_dynamic(connection_id, envelope)
    }

    fn send_response(&self, envelope: proto::Envelope, message_type: &'static str) -> Result<()> {
        log::debug!(
            "rpc respond. client_id:{}, name:{}",
            self.id(),
            message_type
        );
        let connection_id = self.connection_id()?;
        self.peer.send_dynamic(connection_id, envelope)
    }

    fn message_handler_set(&self) -> &Mutex<ProtoMessageHandlerSet> {
        &self.handler_set
    }

    fn has_wsl_interop(&self) -> bool {
        false
    }
}

/// prefix for the wu:// url scheme
pub const ZED_URL_SCHEME: &str = "wu";

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test::FakeServer;

    use gpui::{AppContext as _, TestAppContext};
    use http_client::FakeHttpClient;
    use proto::TypedEnvelope;
    use settings::SettingsStore;

    #[test]
    fn test_proxy_settings_trims_and_ignores_empty_proxy() {
        let mut content = settings::SettingsContent::default();
        content.proxy = Some("   ".to_owned());
        assert_eq!(ProxySettings::from_settings(&content).proxy, None);

        content.proxy = Some("http://127.0.0.1:10809".to_owned());
        assert_eq!(
            ProxySettings::from_settings(&content).proxy.as_deref(),
            Some("http://127.0.0.1:10809")
        );
    }

    #[gpui::test]
    async fn test_subscribing_to_entity(cx: &mut TestAppContext) {
        init_test(cx);
        let client = Client::new(FakeHttpClient::with_404_response());
        let server = FakeServer::for_client(&client, cx).await;

        let (done_tx1, done_rx1) = async_channel::unbounded();
        let (done_tx2, done_rx2) = async_channel::unbounded();
        AnyProtoClient::from(client.clone()).add_entity_message_handler(
            move |entity: Entity<TestEntity>, _: TypedEnvelope<proto::OpenServerSettings>, cx| {
                match entity.read_with(&cx, |entity, _| entity.id) {
                    1 => done_tx1.try_send(()).unwrap(),
                    2 => done_tx2.try_send(()).unwrap(),
                    _ => unreachable!(),
                }
                async { Ok(()) }
            },
        );
        let entity1 = cx.new(|_| TestEntity {
            id: 1,
            subscription: None,
        });
        let entity2 = cx.new(|_| TestEntity {
            id: 2,
            subscription: None,
        });
        let entity3 = cx.new(|_| TestEntity {
            id: 3,
            subscription: None,
        });

        let _subscription1 = client
            .subscribe_to_entity(1)
            .unwrap()
            .set_entity(&entity1, &cx.to_async());
        let _subscription2 = client
            .subscribe_to_entity(2)
            .unwrap()
            .set_entity(&entity2, &cx.to_async());
        // Ensure dropping a subscription for the same entity type still allows receiving of
        // messages for other entity IDs of the same type.
        let subscription3 = client
            .subscribe_to_entity(3)
            .unwrap()
            .set_entity(&entity3, &cx.to_async());
        drop(subscription3);

        server.send(proto::OpenServerSettings { project_id: 1 });
        server.send(proto::OpenServerSettings { project_id: 2 });
        done_rx1.recv().await.unwrap();
        done_rx2.recv().await.unwrap();
    }

    #[gpui::test]
    async fn test_subscribing_after_dropping_subscription(cx: &mut TestAppContext) {
        init_test(cx);
        let client = Client::new(FakeHttpClient::with_404_response());
        let server = FakeServer::for_client(&client, cx).await;

        let entity = cx.new(|_| TestEntity::default());
        let (done_tx1, _done_rx1) = async_channel::unbounded();
        let (done_tx2, done_rx2) = async_channel::unbounded();
        let subscription1 = client.add_message_handler(
            entity.downgrade(),
            move |_, _: TypedEnvelope<proto::Ping>, _| {
                done_tx1.try_send(()).unwrap();
                async { Ok(()) }
            },
        );
        drop(subscription1);
        let _subscription2 = client.add_message_handler(
            entity.downgrade(),
            move |_, _: TypedEnvelope<proto::Ping>, _| {
                done_tx2.try_send(()).unwrap();
                async { Ok(()) }
            },
        );
        server.send(proto::Ping {});
        done_rx2.recv().await.unwrap();
    }

    #[gpui::test]
    async fn test_dropping_subscription_in_handler(cx: &mut TestAppContext) {
        init_test(cx);
        let client = Client::new(FakeHttpClient::with_404_response());
        let server = FakeServer::for_client(&client, cx).await;

        let entity = cx.new(|_| TestEntity::default());
        let (done_tx, done_rx) = async_channel::unbounded();
        let subscription = client.add_message_handler(
            entity.clone().downgrade(),
            move |entity: Entity<TestEntity>, _: TypedEnvelope<proto::Ping>, mut cx| {
                entity
                    .update(&mut cx, |entity, _| entity.subscription.take())
                    .unwrap();
                done_tx.try_send(()).unwrap();
                async { Ok(()) }
            },
        );
        entity.update(cx, |entity, _| {
            entity.subscription = Some(subscription);
        });
        server.send(proto::Ping {});
        done_rx.recv().await.unwrap();
    }

    #[derive(Default)]
    struct TestEntity {
        id: usize,
        subscription: Option<Subscription>,
    }

    fn init_test(cx: &mut TestAppContext) {
        cx.update(|cx| {
            let settings_store = SettingsStore::test(cx);
            cx.set_global(settings_store);
        });
    }
}
