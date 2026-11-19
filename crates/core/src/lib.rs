//! Shared primitives for the open-chat daemon: bounded buffers, supervision.

pub mod buffer;
pub mod supervise;
pub mod tokens;

pub use buffer::RingBuffer;
pub use supervise::{Heartbeat, TaskError, TaskOutcome, spawn_supervised, supervise};
pub use tokens::estimate_tokens;
