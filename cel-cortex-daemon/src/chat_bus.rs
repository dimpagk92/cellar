//! Broadcast bus for `agent.chat.*` frames the agent runtime emits.
//!
//! Each frame carries its `session_id` alongside the IPC payload so the
//! `agent.chat.subscribe` forwarder can filter to one session per
//! subscription.
//!
//! The wire shape is exactly `cellar_ipc::subscription::StreamPayload` —
//! the forwarder wraps each entry in a `StreamFrame` with its
//! `SubscriptionId` and pushes it to the per-connection `FrameSink`.

use cellar_ipc::subscription::StreamPayload;
use tokio::sync::broadcast::{self, Receiver, Sender};

/// One published frame on the chat bus.
#[derive(Debug, Clone)]
pub struct ChatBroadcast {
    /// Which agent session this frame belongs to. Forwarders filter on it.
    pub session_id: String,
    /// The wire payload (Token / ToolCallAttempt / MessageComplete / ...).
    pub payload: StreamPayload,
}

const CHAT_BUS_CAPACITY: usize = 4096;

/// Broadcast bus for chat frames. Cheap to clone (`Arc` underneath).
#[derive(Clone)]
pub struct ChatBus {
    tx: Sender<ChatBroadcast>,
}

impl ChatBus {
    /// New bus with the default capacity.
    pub fn new() -> Self {
        let (tx, _) = broadcast::channel(CHAT_BUS_CAPACITY);
        Self { tx }
    }

    /// Publish one frame.
    pub fn publish(&self, frame: ChatBroadcast) {
        let _ = self.tx.send(frame);
    }

    /// Subscribe.
    pub fn subscribe(&self) -> Receiver<ChatBroadcast> {
        self.tx.subscribe()
    }
}

impl Default for ChatBus {
    fn default() -> Self {
        Self::new()
    }
}
