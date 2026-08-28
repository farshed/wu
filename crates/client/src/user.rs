use super::Client;
use gpui::{Context, SharedString, SharedUri};
use postage::watch;
use std::sync::Arc;

pub type LegacyUserId = u64;

#[derive(Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Clone, Copy)]
pub struct ProjectId(pub u64);

impl ProjectId {
    pub fn to_proto(self) -> u64 {
        self.0
    }
}

#[derive(Default, Debug)]
pub struct User {
    pub legacy_id: LegacyUserId,
    pub username: SharedString,
    pub avatar_uri: SharedUri,
    pub name: Option<String>,
}

impl PartialOrd for User {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for User {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.username.cmp(&other.username)
    }
}

impl PartialEq for User {
    fn eq(&self, other: &Self) -> bool {
        self.legacy_id == other.legacy_id && self.username == other.username
    }
}

impl Eq for User {}

pub struct UserStore {
    current_user: watch::Receiver<Option<Arc<User>>>,
    _current_user_tx: watch::Sender<Option<Arc<User>>>,
}

impl UserStore {
    pub fn new(_client: Arc<Client>, _cx: &Context<Self>) -> Self {
        let (current_user_tx, current_user_rx) = watch::channel();
        Self {
            current_user: current_user_rx,
            _current_user_tx: current_user_tx,
        }
    }

    pub fn current_user(&self) -> Option<Arc<User>> {
        self.current_user.borrow().clone()
    }

    pub fn watch_current_user(&self) -> watch::Receiver<Option<Arc<User>>> {
        self.current_user.clone()
    }
}
