//! Session engine (Roadmap Phases 2 & 4 core).
//!
//! Turns the building blocks into an agent: [`provider`] speaks to
//! OpenAI-compatible models over streaming SSE; [`compositor`] assembles
//! each prompt with hard token budgets and the anti-slop `[during]` block
//! prepended; [`tools`] exposes the governed exec/patch/memory surface;
//! [`agent`] runs the loop, including the autonomous post-patch
//! verification pass that must be green before a turn may close.

pub mod agent;
pub mod compositor;
pub mod events;
pub mod provider;
pub mod slop;
pub mod tools;

pub use agent::{Agent, AgentEvent, AgentOutcome};
pub use compositor::{Composed, Compositor};
pub use events::{Badges, SkillBadge, TurnEvent};
pub use provider::{OpenAi, OpenAiConfig, Provider, ProviderError};
pub use slop::strip_leading_slop;
pub use tools::Registry;
