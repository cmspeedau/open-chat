//! Agent tool layer (Roadmap Phase 2).
//!
//! - [`exec`]: governed command execution — every tool subprocess flows
//!   through the [`openchat_governor::Governor`] (build slot, affinity pin,
//!   env thread caps, niceness, memory headroom) and lands in bounded
//!   [`openchat_core::RingBuffer`]s.
//! - [`patch`]: surgical search/replace blocks — the agent emits only
//!   changed lines, never full files (the 5x output-token fix).
//! - [`compact`]: rolling transcript — old tool logs collapse into
//!   one-line digests so long sessions stay in budget (Phase 1's last item).

pub mod compact;
pub mod diagnose;
pub mod exec;
pub mod patch;
pub mod slop;
