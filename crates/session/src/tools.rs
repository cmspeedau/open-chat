//! The tool surface the model may call, and the registry that dispatches.
//!
//! Every tool here wraps an already-governed primitive; the agent can't
//! reach the machine except through these doors.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex as StdMutex};

use openchat_governor::Governor;
use openchat_memory::{MemoryScope, MemoryStore};
use openchat_tools::compact::ToolRecord;
use openchat_tools::diagnose::{self, Diagnosis};
use openchat_tools::exec::{ExecError, ExecRequest, TaskKind, ToolRunner};
use openchat_tools::patch;
use serde_json::{Value, json};
use tokio::sync::Mutex;

use crate::provider::{BoxFuture, ToolSchema};

/// Ledger of memory faults the diagnostic trap raised during a turn.
/// Filled by `run_command`/`apply_patch`; while armed, the agent injects
/// the memory-safety ruleset into every following prompt (Phase 2's
/// automated skill injection).
#[derive(Debug, Default)]
pub struct Diagnostics {
    armed: AtomicBool,
    count: AtomicUsize,
    last: StdMutex<Option<Diagnosis>>,
    verifications: AtomicUsize,
}

impl Diagnostics {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    pub fn record(&self, d: Diagnosis) -> Diagnosis {
        self.armed.store(true, Ordering::SeqCst);
        self.count.fetch_add(1, Ordering::SeqCst);
        *self.last.lock().expect("diagnostics lock") = Some(d.clone());
        d
    }

    #[must_use]
    pub fn is_armed(&self) -> bool {
        self.armed.load(Ordering::SeqCst)
    }

    #[must_use]
    pub fn count(&self) -> usize {
        self.count.load(Ordering::SeqCst)
    }

    #[must_use]
    pub fn last(&self) -> Option<Diagnosis> {
        self.last.lock().expect("diagnostics lock").clone()
    }

    /// Count an actually-executed verification pass. Badges report
    /// `checks_run` from this, so a selected-but-unused verifier is never
    /// claimed as run.
    pub fn note_verification(&self) {
        self.verifications.fetch_add(1, Ordering::SeqCst);
    }

    #[must_use]
    pub fn verifications(&self) -> usize {
        self.verifications.load(Ordering::SeqCst)
    }
}

/// Surfaces bound to the current turn (code, later ui/copy), with the
/// reason each was armed. Filled by `apply_patch` source touches and by
/// the daemon from the stack fingerprint; the agent injects the matching
/// ruleset into following prompts and reports them on the badges.
#[derive(Debug, Default)]
pub struct Surfaces {
    armed: StdMutex<Vec<(String, String)>>,
    findings: AtomicUsize,
}

impl Surfaces {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Arm a surface; true when newly armed (first reason wins, deduped).
    pub fn arm(&self, surface: &str, reason: impl Into<String>) -> bool {
        let mut guard = self.armed.lock().expect("surfaces lock");
        if guard.iter().any(|(s, _)| s == surface) {
            return false;
        }
        guard.push((surface.to_owned(), reason.into()));
        true
    }

    #[must_use]
    pub fn is_armed(&self, surface: &str) -> bool {
        self.armed
            .lock()
            .expect("surfaces lock")
            .iter()
            .any(|(s, _)| s == surface)
    }

    #[must_use]
    pub fn list(&self) -> Vec<(String, String)> {
        self.armed.lock().expect("surfaces lock").clone()
    }

    /// Total advisory findings raised by surface gates this turn.
    pub fn note_findings(&self, n: usize) {
        self.findings.fetch_add(n, Ordering::SeqCst);
    }

    #[must_use]
    pub fn findings(&self) -> usize {
        self.findings.load(Ordering::SeqCst)
    }
}

/// What a tool returns to the model, plus an optional transcript record
/// for the rolling compactor.
#[derive(Debug, Clone)]
pub struct ToolOutcome {
    pub ok: bool,
    pub text: String,
    pub record: Option<ToolRecord>,
}

impl ToolOutcome {
    #[must_use]
    pub fn plain(ok: bool, text: impl Into<String>) -> Self {
        Self {
            ok,
            text: text.into(),
            record: None,
        }
    }
}

/// A model-visible tool.
pub trait Tool: Send + Sync {
    fn schema(&self) -> ToolSchema;
    fn call<'a>(&'a self, args: Value) -> BoxFuture<'a, ToolOutcome>;
    /// Downcast hook so the agent can reach typed tools (verification
    /// state) that live in the registry behind `dyn Tool`.
    fn as_any(&self) -> &dyn std::any::Any;
}

/// Registry: name -> tool, plus the schema list for requests.
#[derive(Default)]
pub struct Registry {
    tools: Vec<(String, Box<dyn Tool>)>,
}

impl Registry {
    pub fn register(&mut self, tool: Box<dyn Tool>) {
        self.tools.push((tool.schema().name.clone(), tool));
    }

    pub fn iter(&self) -> impl Iterator<Item = &dyn Tool> {
        self.tools.iter().map(|(_, t)| t.as_ref())
    }

    #[must_use]
    pub fn schemas(&self) -> Vec<ToolSchema> {
        self.tools.iter().map(|(_, t)| t.schema()).collect()
    }

    #[must_use]
    pub fn names(&self) -> Vec<String> {
        self.tools.iter().map(|(n, _)| n.clone()).collect()
    }

    /// Dispatch an unknown call is an `ok=false` result (fed back to the
    /// model), never a panic or hard error.
    pub async fn dispatch(&self, call: &crate::provider::ToolCall) -> ToolOutcome {
        let args: Value = match serde_json::from_str(&call.arguments) {
            Ok(v) => v,
            Err(e) => {
                return ToolOutcome::plain(false, format!("arguments are not valid JSON: {e}"));
            }
        };
        match self.tools.iter().find(|(n, _)| *n == call.name) {
            Some((_, tool)) => {
                tracing::debug!(tool = %call.name, "tool call");
                tool.call(args).await
            }
            None => ToolOutcome::plain(
                false,
                format!(
                    "unknown tool {:?}; available: {:?}",
                    call.name,
                    self.names()
                ),
            ),
        }
    }
}

/// Runs a shell-quoted-free command line under full governance.
pub struct RunCommandTool {
    governor: Arc<Governor>,
    root: PathBuf,
    timeout: std::time::Duration,
    diagnostics: Arc<Diagnostics>,
}

impl RunCommandTool {
    #[must_use]
    pub const fn new(
        governor: Arc<Governor>,
        root: PathBuf,
        timeout: std::time::Duration,
        diagnostics: Arc<Diagnostics>,
    ) -> Self {
        Self {
            governor,
            root,
            timeout,
            diagnostics,
        }
    }
}

impl Tool for RunCommandTool {
    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: "run_command".into(),
            description: "Run one command in the workspace. Whitespace-split argv, no shell. \
                          Builds/tests take the shared build slot; keep output small."
                .into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "cmd": { "type": "string", "description": "command line, e.g. `cargo check --workspace`" },
                    "build": { "type": "boolean", "description": "true (default) to occupy a build slot; false for quick reads like `cat`" },
                },
                "required": ["cmd"],
                "additionalProperties": false,
            }),
        }
    }

    fn as_any(&self) -> &dyn std::any::Any {
        self
    }

    fn call(&self, args: Value) -> BoxFuture<'_, ToolOutcome> {
        Box::pin(async move {
            let Some(cmd) = args.get("cmd").and_then(Value::as_str) else {
                return ToolOutcome::plain(false, "missing `cmd` string");
            };
            let mut parts = cmd.split_whitespace();
            let Some(program) = parts.next() else {
                return ToolOutcome::plain(false, "empty command");
            };
            // Heuristic default: cheap filesystem reads don't need a slot.
            let light_hits = [
                "cat", "head", "tail", "ls", "rg", "grep", "find", "wc", "sed",
            ];
            let default_build = !light_hits.contains(&program);
            let build = args
                .get("build")
                .and_then(Value::as_bool)
                .unwrap_or(default_build);
            let req = ExecRequest {
                program: program.into(),
                args: parts.map(str::to_owned).collect(),
                cwd: Some(self.root.clone()),
                kind: if build {
                    TaskKind::Build
                } else {
                    TaskKind::Light
                },
                timeout: self.timeout,
                output_cap: openchat_tools::exec::DEFAULT_OUTPUT_CAP,
            };
            match ToolRunner::new(&self.governor).run(&req).await {
                Ok(report) => {
                    let record = ToolRecord::from_report(cmd, &report);
                    let ok = record.succeeded();
                    let mut text = render_exec(&record);
                    // The diagnostic trap: classify memory failures the exit
                    // code alone hides, arm the skill injection, and show
                    // the model the evidence plus the next step.
                    if let Some(d) = diagnose::diagnose(
                        &record.stdout,
                        &record.stderr,
                        record.exit_code,
                        record.signal,
                        record.timed_out,
                    ) {
                        let d = self.diagnostics.record(d);
                        text.push('\n');
                        text.push_str(&d.render());
                    }
                    ToolOutcome {
                        ok,
                        text,
                        record: Some(record),
                    }
                }
                Err(ExecError::NoHeadroom) => {
                    let d = self.diagnostics.record(diagnose::headroom_refused());
                    ToolOutcome::plain(false, d.render())
                }
                Err(e) => ToolOutcome::plain(false, format!("exec error: {e}")),
            }
        })
    }
}

/// Exec result rendered for the model: digest line plus output tails,
/// clamped so one tool message cannot eat the context window.
fn render_exec(record: &ToolRecord) -> String {
    const TAIL: usize = 4096;
    let mut out = record.digest();
    let tail = |s: &str| -> String {
        if s.len() <= TAIL {
            s.to_owned()
        } else {
            let mut cut = s.len() - TAIL;
            while !s.is_char_boundary(cut) {
                cut += 1;
            }
            s[cut..].to_owned()
        }
    };
    if !record.stdout.is_empty() {
        out.push_str("\n<stdout>\n");
        out.push_str(&tail(&record.stdout));
        out.push('\n');
    }
    if !record.stderr.is_empty() {
        out.push_str("\n<stderr>\n");
        out.push_str(&tail(&record.stderr));
        out.push('\n');
    }
    out
}

/// Applies SEARCH/REPLACE blocks, then (Phase 2 item 3) runs the project's
/// verification command automatically and reports its verdict in the same
/// tool result, so a broken patch keeps the turn open.
pub struct ApplyPatchTool {
    root: PathBuf,
    governor: Arc<Governor>,
    verifier: Option<Vec<String>>,
    patched_this_turn: std::sync::atomic::AtomicBool,
    diagnostics: Arc<Diagnostics>,
    surfaces: Arc<Surfaces>,
}

impl ApplyPatchTool {
    #[must_use]
    pub fn new(
        root: PathBuf,
        governor: Arc<Governor>,
        verifier: Option<Vec<String>>,
        diagnostics: Arc<Diagnostics>,
        surfaces: Arc<Surfaces>,
    ) -> Self {
        Self {
            root,
            governor,
            verifier,
            patched_this_turn: std::sync::atomic::AtomicBool::new(false),
            diagnostics,
            surfaces,
        }
    }

    #[must_use]
    pub fn is_dirty(&self) -> bool {
        self.patched_this_turn
            .load(std::sync::atomic::Ordering::SeqCst)
    }

    /// Called by the agent loop once a turn-closing verification passes.
    pub fn clear_dirty(&self) {
        self.patched_this_turn
            .store(false, std::sync::atomic::Ordering::SeqCst);
    }

    /// Run the configured verification command (build-slot governed).
    pub async fn verify(&self) -> Option<ToolRecord> {
        let verb = self.verifier.as_ref()?;
        self.diagnostics.note_verification();
        let (program, args) = verb.split_first()?;
        let req = ExecRequest {
            program: program.clone(),
            args: args.to_vec(),
            cwd: Some(self.root.clone()),
            kind: TaskKind::Build,
            timeout: std::time::Duration::from_secs(600),
            output_cap: openchat_tools::exec::DEFAULT_OUTPUT_CAP,
        };
        match ToolRunner::new(&self.governor).run(&req).await {
            Ok(report) => {
                let record = ToolRecord::from_report(verb.join(" "), &report);
                // Verifiers are where sanitizers run: trap there too.
                if let Some(d) = diagnose::diagnose(
                    &record.stdout,
                    &record.stderr,
                    record.exit_code,
                    record.signal,
                    record.timed_out,
                ) {
                    self.diagnostics.record(d);
                }
                Some(record)
            }
            Err(e) => Some(ToolRecord {
                label: verb.join(" "),
                exit_code: None,
                signal: None,
                timed_out: false,
                duration_ms: 0,
                stdout: String::new(),
                stderr: format!("verifier could not start: {e}"),
                dropped: 0,
            }),
        }
    }
}

impl Tool for ApplyPatchTool {
    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: "apply_patch".into(),
            description: "Apply surgical SEARCH/REPLACE blocks. Format per file:\n\
                          @@@ relative/path\n<<<<<<< SEARCH\nold lines\n=======\n\
                          new lines\n>>>>>>> REPLACE\n\
                          Empty SEARCH creates the file. Never send whole-file contents."
                .into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "patches": { "type": "string", "description": "one or more @@@ sections" },
                },
                "required": ["patches"],
                "additionalProperties": false,
            }),
        }
    }

    fn as_any(&self) -> &dyn std::any::Any {
        self
    }

    fn call(&self, args: Value) -> BoxFuture<'_, ToolOutcome> {
        Box::pin(async move {
            let Some(blocks) = args.get("patches").and_then(Value::as_str) else {
                return ToolOutcome::plain(false, "missing `patches` string");
            };
            let parsed = patch::parse(blocks);
            if parsed.is_empty() {
                return ToolOutcome::plain(
                    false,
                    "no @@@ sections parsed; expected `@@@ path` followed by SEARCH/REPLACE blocks",
                );
            }
            let applied = match patch::apply_to_disk(&self.root, &parsed) {
                Ok(a) => a,
                Err(e) => return ToolOutcome::plain(false, format!("patch rejected: {e}")),
            };
            let mut text = applied
                .iter()
                .map(|a| {
                    format!(
                        "{} ({} lines changed)\n{}",
                        a.path.display(),
                        a.diff.matches('\n').count(),
                        a.diff
                    )
                })
                .collect::<Vec<_>>()
                .join("\n");
            // Code-surface gate: touching source arms the ruleset for the
            // rest of the turn, and added lines get the deterministic
            // slop checks (swallowed errors, decorative comments).
            // Operation-aware auto-binding (Phase 3): each touched file
            // maps to its task surfaces; armed surfaces inject their
            // ruleset into following prompts and their added lines run
            // through the matching patch-time gate.
            let mut touched: Vec<(&str, Vec<String>)> = Vec::new(); // surface -> files
            for file in &parsed {
                for surface in openchat_tools::slop::surfaces_of(&file.path) {
                    if let Some((_, files)) = touched.iter_mut().find(|(s, _)| *s == *surface) {
                        files.push(file.path.display().to_string());
                    } else {
                        touched.push((surface, vec![file.path.display().to_string()]));
                    }
                }
            }
            for (surface, files) in &touched {
                self.surfaces
                    .arm(surface, format!("patch touched: {}", files.join(", ")));
            }
            let mut advisories = Vec::new();
            for file in &parsed {
                let added = file
                    .hunks
                    .iter()
                    .map(|h| h.replace.as_str())
                    .collect::<Vec<_>>()
                    .join("\n");
                let mut hits = openchat_tools::slop::check_added_source_text(&file.path, &added);
                hits.extend(openchat_tools::slop::check_added_ui_text(
                    &file.path, &added,
                ));
                hits.extend(openchat_tools::slop::check_prose_text(&added));
                for finding in hits {
                    advisories.push(format!("{}: {}", file.path.display(), finding.render()));
                }
            }
            if !advisories.is_empty() {
                self.surfaces.note_findings(advisories.len());
                text.push_str("\n\n[code-slop advisory - not a failure, fix if avoidable]\n");
                text.push_str(&advisories.join("\n"));
            }
            // Run the formatters the project actually has; disclose the rest.
            let touched_files: Vec<std::path::PathBuf> = parsed
                .iter()
                .filter(|f| !f.hunks.iter().any(|h| h.delete))
                .map(|f| f.path.clone())
                .collect();
            if !touched_files.is_empty() {
                let results = openchat_tools::format::format_files(&self.root, &touched_files);
                let mut formatted = Vec::new();
                let mut unavailable = Vec::new();
                for (path, outcome) in results {
                    match outcome {
                        openchat_tools::format::FormatOutcome::Formatted(tool) => {
                            formatted.push(format!("{path} ({tool})"));
                        }
                        openchat_tools::format::FormatOutcome::NoFormatter => {}
                        openchat_tools::format::FormatOutcome::Unavailable(tool) => {
                            unavailable.push(format!("{path}: no {tool}"));
                        }
                        openchat_tools::format::FormatOutcome::Failed(err) => {
                            unavailable.push(format!("{path}: {err}"));
                        }
                    }
                }
                if !formatted.is_empty() {
                    text.push_str(&format!("\n\n[formatted] {}", formatted.join(", ")));
                }
                if !unavailable.is_empty() {
                    text.push_str(&format!(
                        "\n[formatter unavailable] {}",
                        unavailable.join("; ")
                    ));
                }
            }
            // Autonomous verification, right here, before the model sees
            // the result of its own edit. A pass clears dirty; a failure
            // leaves the turn open for the close-gate's repair nudge.
            match self.verify().await {
                Some(record) => {
                    let ok = record.succeeded();
                    self.patched_this_turn
                        .store(!ok, std::sync::atomic::Ordering::SeqCst);
                    text.push_str("\n\n[post-patch verification] ");
                    text.push_str(&render_exec(&record));
                    ToolOutcome {
                        ok,
                        text,
                        record: Some(record),
                    }
                }
                None => ToolOutcome::plain(true, text),
            }
        })
    }
}

/// Shared handle: `MemoryStore` wraps a `Connection` (not Sync), so tools
/// take it locked. The daemon hands the same Arc to all memory tools.
pub type SharedMemory = Arc<Mutex<MemoryStore>>;

fn parse_scope(v: Option<&Value>) -> MemoryScope {
    match v.and_then(Value::as_str) {
        Some("global") => MemoryScope::Global,
        Some("session") => MemoryScope::Session,
        _ => MemoryScope::Project,
    }
}

pub struct MemoryStoreTool {
    store: SharedMemory,
}

impl MemoryStoreTool {
    #[must_use]
    pub const fn new(store: SharedMemory) -> Self {
        Self { store }
    }
}

impl Tool for MemoryStoreTool {
    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: "memory_store".into(),
            description:
                "Persist a durable fact keyed by (scope, key). Re-storing a key overwrites it."
                    .into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "key": { "type": "string" },
                    "value": { "type": "string" },
                    "category": { "type": "string", "description": "defaults to `general`" },
                    "scope": { "type": "string", "enum": ["project", "global", "session"] },
                },
                "required": ["key", "value"],
                "additionalProperties": false,
            }),
        }
    }

    fn as_any(&self) -> &dyn std::any::Any {
        self
    }

    fn call(&self, args: Value) -> BoxFuture<'_, ToolOutcome> {
        Box::pin(async move {
            let (Some(key), Some(value)) = (
                args.get("key").and_then(Value::as_str),
                args.get("value").and_then(Value::as_str),
            ) else {
                return ToolOutcome::plain(false, "missing key/value strings");
            };
            let entry = openchat_memory::MemoryEntry {
                scope: parse_scope(args.get("scope")),
                category: args
                    .get("category")
                    .and_then(Value::as_str)
                    .unwrap_or("general")
                    .to_owned(),
                key: key.to_owned(),
                value: value.to_owned(),
                embedding: None, // dense vectors arrive with the embedding provider
            };
            match self.store.lock().await.store(&entry) {
                Ok(id) => ToolOutcome::plain(
                    true,
                    format!("stored #{id} [{}:{key}]", entry.scope.as_str()),
                ),
                Err(e) => ToolOutcome::plain(false, format!("memory store failed: {e}")),
            }
        })
    }
}

pub struct MemoryRecallTool {
    store: SharedMemory,
    budget_tokens: usize,
}

impl MemoryRecallTool {
    #[must_use]
    pub const fn new(store: SharedMemory, budget_tokens: usize) -> Self {
        Self {
            store,
            budget_tokens,
        }
    }
}

impl Tool for MemoryRecallTool {
    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: "memory_recall".into(),
            description: "Keyword recall over stored memory. The system also injects top hits automatically; use this for follow-ups."
                .into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "query": { "type": "string" },
                },
                "required": ["query"],
                "additionalProperties": false,
            }),
        }
    }

    fn as_any(&self) -> &dyn std::any::Any {
        self
    }

    fn call(&self, args: Value) -> BoxFuture<'_, ToolOutcome> {
        Box::pin(async move {
            let Some(query) = args.get("query").and_then(Value::as_str) else {
                return ToolOutcome::plain(false, "missing query string");
            };
            match self
                .store
                .lock()
                .await
                .recall(query, None, self.budget_tokens)
            {
                Ok(hits) if hits.is_empty() => ToolOutcome::plain(true, "no matching memories"),
                Ok(hits) => ToolOutcome::plain(
                    true,
                    hits.iter()
                        .map(|h| {
                            format!(
                                "[{}:{}] {}: {}",
                                h.entry.scope.as_str(),
                                h.entry.category,
                                h.entry.key,
                                h.entry.value
                            )
                        })
                        .collect::<Vec<_>>()
                        .join("\n"),
                ),
                Err(e) => ToolOutcome::plain(false, format!("recall failed: {e}")),
            }
        })
    }
}

pub struct MemoryForgetTool {
    store: SharedMemory,
}

impl MemoryForgetTool {
    #[must_use]
    pub const fn new(store: SharedMemory) -> Self {
        Self { store }
    }
}

impl Tool for MemoryForgetTool {
    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: "memory_forget".into(),
            description: "Delete a stored memory by (scope, key).".into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "key": { "type": "string" },
                    "scope": { "type": "string", "enum": ["project", "global", "session"] },
                },
                "required": ["key"],
                "additionalProperties": false,
            }),
        }
    }

    fn as_any(&self) -> &dyn std::any::Any {
        self
    }

    fn call(&self, args: Value) -> BoxFuture<'_, ToolOutcome> {
        Box::pin(async move {
            let Some(key) = args.get("key").and_then(Value::as_str) else {
                return ToolOutcome::plain(false, "missing key string");
            };
            match self
                .store
                .lock()
                .await
                .forget(parse_scope(args.get("scope")), key)
            {
                Ok(true) => ToolOutcome::plain(true, format!("forgot {key}")),
                Ok(false) => ToolOutcome::plain(true, format!("nothing stored under {key}")),
                Err(e) => ToolOutcome::plain(false, format!("forget failed: {e}")),
            }
        })
    }
}

/// Build the standard registry used by the daemon and the agent loop.
/// `diagnostics` receives memory-fault traps from both exec and verify.
#[must_use]
pub fn standard_registry(
    governor: Arc<Governor>,
    root: &Path,
    memory: SharedMemory,
    verifier: Option<Vec<String>>,
    diagnostics: Arc<Diagnostics>,
    surfaces: Arc<Surfaces>,
) -> Registry {
    let mut reg = Registry::default();
    reg.register(Box::new(RunCommandTool::new(
        Arc::clone(&governor),
        root.to_path_buf(),
        std::time::Duration::from_secs(300),
        Arc::clone(&diagnostics),
    )));
    reg.register(Box::new(ApplyPatchTool::new(
        root.to_path_buf(),
        governor,
        verifier,
        diagnostics,
        surfaces,
    )));
    reg.register(Box::new(MemoryStoreTool::new(Arc::clone(&memory))));
    reg.register(Box::new(MemoryRecallTool::new(Arc::clone(&memory), 800)));
    reg.register(Box::new(MemoryForgetTool::new(memory)));
    reg
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::provider::ToolCall;
    use std::sync::Arc;

    fn governor() -> Arc<Governor> {
        Arc::new(
            Governor::new(openchat_governor::GovernorConfig {
                low_process_priority: false,
                ..Default::default()
            })
            .unwrap(),
        )
    }

    async fn mem() -> SharedMemory {
        Arc::new(Mutex::new(MemoryStore::open_in_memory(4).unwrap()))
    }

    fn call(name: &str, arguments: &str) -> ToolCall {
        ToolCall {
            id: "c1".into(),
            name: name.into(),
            arguments: arguments.into(),
        }
    }

    #[tokio::test]
    async fn run_command_executes_and_digests() {
        let g = governor();
        let reg = standard_registry(
            Arc::clone(&g),
            Path::new("/"),
            mem().await,
            None,
            Arc::new(Diagnostics::new()),
            Arc::new(Surfaces::new()),
        );
        let out = reg
            .dispatch(&call("run_command", r#"{"cmd":"echo hi","build":false}"#))
            .await;
        assert!(out.ok);
        assert!(out.text.contains("$ echo hi → ok"), "{}", out.text);
        assert!(out.text.contains("<stdout>\nhi"));
        assert!(out.record.is_some());
    }

    #[tokio::test]
    async fn unknown_tool_is_a_soft_error() {
        let reg = standard_registry(
            governor(),
            Path::new("/tmp"),
            mem().await,
            None,
            Arc::new(Diagnostics::new()),
            Arc::new(Surfaces::new()),
        );
        let out = reg.dispatch(&call("launch_missiles", "{}")).await;
        assert!(!out.ok);
        assert!(out.text.contains("unknown tool"), "{}", out.text);
    }

    #[tokio::test]
    async fn bad_json_arguments_rejected() {
        let reg = standard_registry(
            governor(),
            Path::new("/tmp"),
            mem().await,
            None,
            Arc::new(Diagnostics::new()),
            Arc::new(Surfaces::new()),
        );
        let out = reg.dispatch(&call("run_command", "{not json")).await;
        assert!(!out.ok);
        assert!(out.text.contains("not valid JSON"));
    }

    #[tokio::test]
    async fn patch_applies_then_verifies() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("f.txt"), "one\n").unwrap();
        let g = governor();
        // Verifier: grep for "two" so we can watch pass and fail.
        let reg = standard_registry(
            g,
            dir.path(),
            mem().await,
            Some(vec![
                "grep".into(),
                "-q".into(),
                "two".into(),
                "f.txt".into(),
            ]),
            Arc::new(Diagnostics::new()),
            Arc::new(Surfaces::new()),
        );
        let failing = reg
            .dispatch(&call(
                "apply_patch",
                &json!({"patches": "@@@ f.txt\n<<<<<<< SEARCH\none\n=======\nuno\n>>>>>>> REPLACE\n"}).to_string(),
            ))
            .await;
        assert!(
            !failing.ok,
            "verification failure must mark the tool result failed"
        );
        assert!(failing.text.contains("post-patch verification"));
        assert!(
            std::fs::read_to_string(dir.path().join("f.txt")).unwrap() == "uno\n",
            "patch applied before verify"
        );

        let passing = reg
            .dispatch(&call(
                "apply_patch",
                &json!({"patches": "@@@ f.txt\n<<<<<<< SEARCH\nuno\n=======\ntwo\n>>>>>>> REPLACE\n"}).to_string(),
            ))
            .await;
        assert!(passing.ok);
    }

    #[tokio::test]
    async fn memory_round_trip_through_tools() {
        let store = mem().await;
        let reg = standard_registry(
            governor(),
            Path::new("/tmp"),
            Arc::clone(&store),
            None,
            Arc::new(Diagnostics::new()),
            Arc::new(Surfaces::new()),
        );
        let stored = reg
            .dispatch(&call(
                "memory_store",
                r#"{"key":"style","value":"tabs not spaces","category":"conventions"}"#,
            ))
            .await;
        assert!(stored.ok, "{}", stored.text);
        let found = reg
            .dispatch(&call("memory_recall", r#"{"query":"tabs"}"#))
            .await;
        assert!(found.ok && found.text.contains("tabs not spaces"));
        let gone = reg
            .dispatch(&call("memory_forget", r#"{"key":"style"}"#))
            .await;
        assert!(gone.ok && gone.text.contains("forgot"));
        let empty = reg
            .dispatch(&call("memory_recall", r#"{"query":"tabs"}"#))
            .await;
        assert!(empty.text.contains("no matching memories"));
    }

    #[tokio::test]
    async fn sanitizer_report_arms_ledger_and_annotates_result() {
        let g = governor();
        let diag = Arc::new(Diagnostics::new());
        let reg = standard_registry(
            g,
            Path::new("/tmp"),
            mem().await,
            None,
            Arc::clone(&diag),
            Arc::new(Surfaces::new()),
        );
        // No shell needed: a file with the sanitizer banner, printed by cat.
        let dir = tempfile::tempdir().unwrap();
        let log = dir.path().join("asan.log");
        std::fs::write(
            &log,
            "==42==ERROR: AddressSanitizer: heap-use-after-free on address 0xdeadbeef\n",
        )
        .unwrap();
        let out = reg
            .dispatch(&call(
                "run_command",
                &json!({"cmd": format!("cat {}", log.display()), "build": false}).to_string(),
            ))
            .await;
        // cat itself succeeded; the trap fired on the banner it printed.
        assert!(out.ok);
        assert!(
            out.text
                .contains("[memory-diagnostic] fault=address-sanitizer"),
            "{}",
            out.text
        );
        assert!(diag.is_armed());
        assert_eq!(diag.count(), 1);
        assert_eq!(
            diag.last().unwrap().fault,
            openchat_tools::diagnose::MemoryFault::AddressSanitizer
        );
        // A clean command leaves the ledger untouched.
        let clean = reg
            .dispatch(&call("run_command", r#"{"cmd":"true","build":false}"#))
            .await;
        assert!(clean.ok);
        assert_eq!(diag.count(), 1);
    }

    #[tokio::test]
    async fn source_patch_arms_code_surface_and_advises_on_added_slop() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("m.ts"), "run();\n").unwrap();
        let surfaces = Arc::new(Surfaces::new());
        let reg = standard_registry(
            governor(),
            dir.path(),
            mem().await,
            None,
            Arc::new(Diagnostics::new()),
            Arc::clone(&surfaces),
        );
        let out = reg
            .dispatch(&call(
                "apply_patch",
                &json!({"patches": "@@@ m.ts\n<<<<<<< SEARCH\nrun();\n=======\ntry { run(); } catch (e) {}\n// ================= SETUP =================\n>>>>>>> REPLACE\n"}).to_string(),
            ))
            .await;
        assert!(out.ok, "{}", out.text);
        assert!(surfaces.is_armed("code"), "source touch must arm");
        assert!(
            out.text.contains("[code-slop advisory")
                && out.text.contains("swallowed-error (empty catch)")
                && out.text.contains("decorative comment"),
            "advisories missing: {}",
            out.text
        );
    }

    #[tokio::test]
    async fn docs_only_patch_does_not_arm_code_surface() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("notes.md"), "keep\n").unwrap();
        let surfaces = Arc::new(Surfaces::new());
        let reg = standard_registry(
            governor(),
            dir.path(),
            mem().await,
            None,
            Arc::new(Diagnostics::new()),
            Arc::clone(&surfaces),
        );
        let out = reg
            .dispatch(&call(
                "apply_patch",
                &json!({"patches": "@@@ notes.md\n<<<<<<< SEARCH\nkeep\n=======\nkept\n>>>>>>> REPLACE\n"}).to_string(),
            ))
            .await;
        assert!(out.ok);
        assert!(!surfaces.is_armed("code"), "docs are not a code surface");
    }

    #[tokio::test]
    async fn css_patch_arms_ui_surface_with_gate_advisory() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("btn.css"), "button { color: red; }\n").unwrap();
        let surfaces = Arc::new(Surfaces::new());
        let reg = standard_registry(
            governor(),
            dir.path(),
            mem().await,
            None,
            Arc::new(Diagnostics::new()),
            Arc::clone(&surfaces),
        );
        let out = reg
            .dispatch(&call(
                "apply_patch",
                &json!({"patches": "@@@ btn.css\n<<<<<<< SEARCH\nbutton { color: red; }\n=======\nbutton { outline: none; }\n>>>>>>> REPLACE\n"}).to_string(),
            ))
            .await;
        assert!(surfaces.is_armed("ui"));
        assert!(!surfaces.is_armed("code"), "css is ui-only per the matrix");
        assert!(out.text.contains("focus indicator removed"), "{}", out.text);
        assert!(surfaces.findings() >= 1);
    }

    #[tokio::test]
    async fn formatter_runs_after_rs_patch() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("Cargo.toml"), "[package]\nname=\"t\"\n").unwrap();
        std::fs::write(
            dir.path().join("m.rs"),
            "pub fn  x(){}
",
        )
        .unwrap();
        let reg = standard_registry(
            governor(),
            dir.path(),
            mem().await,
            None,
            Arc::new(Diagnostics::new()),
            Arc::new(Surfaces::new()),
        );
        let out = reg
            .dispatch(&call(
                "apply_patch",
                &json!({"patches": "@@@ m.rs\n<<<<<<< SEARCH\npub fn  x(){}\n=======\npub fn  x(){ let _ = 1 ;}\n>>>>>>> REPLACE\n"}).to_string(),
            ))
            .await;
        assert!(out.ok, "{}", out.text);
        assert!(
            out.text.contains("[formatted]"),
            "rustfmt should have run: {}",
            out.text
        );
        let after = std::fs::read_to_string(dir.path().join("m.rs")).unwrap();
        assert!(after.contains("pub fn x() {"), "formatted source: {after}");
    }

    #[tokio::test]
    async fn unsafe_paths_blocked_in_patch_tool() {
        let reg = standard_registry(
            governor(),
            Path::new("/tmp/root"),
            mem().await,
            None,
            Arc::new(Diagnostics::new()),
            Arc::new(Surfaces::new()),
        );
        let out = reg
            .dispatch(&call(
                "apply_patch",
                &json!({"patches": "@@@ ../../etc/passwd\n<<<<<<< SEARCH\nx\n=======\ny\n>>>>>>> REPLACE\n"}).to_string(),
            ))
            .await;
        assert!(!out.ok);
        assert!(out.text.contains("patch rejected"), "{}", out.text);
    }
}
