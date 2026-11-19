//! Autonomous skill binding (Roadmap Phase 3).
//!
//! [`fingerprint`] detects the workspace's languages from manifests and
//! file extensions; [`registry`] indexes `SKILL.md` packages from the
//! skill roots; [`bind`] maps one onto the other and reports *gaps*,
//! languages with no matching skill. The watcher bus decides *when* to
//! re-run this via [`bind::affects_fingerprint`].

pub mod bind;
pub mod fingerprint;
pub mod prompt;
pub mod registry;
pub mod synthesize;

pub use bind::{Binding, Gap, bind};
pub use fingerprint::{Fingerprint, Language, fingerprint};
pub use prompt::{PromptSkill, prompt_set};
pub use registry::{Skill, load_roots};
pub use synthesize::{bootstrap, template};
