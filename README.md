# open-chat

Next-generation agentic workspace & rewrite inspired by [OpenChamber](https://github.com/openchamber/openchamber).

`open-chat` transitions from an Electron/mobile-heavy model to a **headless, ultra-fast core daemon** supporting first-class multi-interface clients (Telegram, Discord, Web) with **native persistent memory**, **autonomous 50+ skill auto-binding**, **interactive skill gap synthesis**, **surgical diff patching (solving 5× output token bloat)**, **hardcoded Anti-Slop Law compiled into core engine logic**, **runtime memory bug diagnostic self-healing**, **subagent deadlock prevention**, **system lockup prevention with user-configurable CPU core throttling changeable via command on the fly**, and a **Context Compositor engine**.

---

## 🔗 Reference Projects & Ecosystem

- **Base Reference:** [openchamber/openchamber](https://github.com/openchamber/openchamber) — Headless core architecture decoupling the engine from mobile/Electron wrappers.
- **Skill Reference:** [miqdadbadjuber/anti-slop](https://github.com/miqdadbadjuber/anti-slop) — Exemplar ruleset illustrating stage-based operational modes (`during` vs `after`) across design, copy, and code generation.

---

## ⚖️ Embedded Anti-Slop: Made Into Inviolable Engine Law

In standard agent setups, anti-slop guidelines are treated as optional system prompt advice that LLMs frequently ignore under high context pressure. 

In `open-chat`, **Anti-Slop is hardcoded into the Rust daemon binary as Inviolable Engine Law (`const ANTI_SLOP_LAW`)**. It cannot be overridden by conversational drift, user prompts, or subagent prompts.

### The Five Engine Invariants (The "Anti-Slop Law")

```rust
// Core Engine Invariant: Embedded at compile-time directly in the Rust binary
pub const ANTI_SLOP_LAW: &str = r#"
[INVIOLABLE ENGINE LAW - ZERO-TOLERANCE ANTI-SLOP]
1. SIGNAL OVER CEREMONY: No throat-clearing, pleasantries, apologies, self-narration, or redundant post-action summaries; communicate only useful results.
2. SURGICAL, TASK-SIZED CHANGES: Prefer minimal diffs. Never re-emit an unchanged file when a patch suffices; do not add defensive layers, abstractions, speculative generality, wrappers, catches, or micro-optimizations without a demonstrated need.
3. VERIFY, THEN REPORT: Run relevant compiler, tests, and linters where available; inspect their actual output, fix actionable failures, and never claim checks passed when they did not.
4. FIT THE ACTIVE SURFACE: Bind relevant Code, UI/Design, Copy, and Human-style skills to touched files and tasks. Preserve project conventions; do not invent imports, dependencies, APIs, facts, or functionality.
5. DELIVER WITH EVIDENCE: For edits, return a concise result and the verification actually performed. Do not grade your own work or narrate imagined checks.
"#;
```

```
                        LLM Stream Chunk
                               │
                               ▼
┌──────────────────────────────────────────────────────────────┐
│           Rust Core Stream Interceptor & Sanitizer           │
│                                                              │
│  1. Pre-Execution Enforcer: Injects Law into system frame #0 │
│  2. Stream Slop Trimmer: Cuts pleasantries before user sees  │
│  3. Diff Validator: Rejects full-file rewrite tool calls     │
│  4. Post-Hoc Slop Penalty: Deducts budget if rules violated  │
└──────────────────────────────────────────────────────────────┘
                               │
                               ▼
            Clean, High-Density, Pure Signal Stream
```

### 1. Compile-Time Inviolability (`System Frame 0`)
The `ANTI_SLOP_LAW` is baked directly into the daemon crate via `include_str!`. The Context Compositor injects it into **System Frame 0** above all user prompts and loaded project skills. Even if a model is tempted to be conversational, the engine frames it as an unbypassable execution constraint.

### 2. Stream-Level Slop Stripping (Zero-Latency Engine Filter)
If a model starts generating conversational fluff (e.g., *"Certainly! Here is the updated code:"*), the streaming response engine buffers the leading 40 tokens through an internal Trie filter. If conversational slop is detected, the daemon **silently drops those tokens from the stream** and only emits the actual tool call or diff block to the client.

### 3. Tool-Level Full-File Rewrite Rejection
The engine's `file_patch` tool actively rejects attempts to overwrite entire files when a surgical diff would suffice. If an agent tries to rewrite a 400-line file to update 3 lines, the tool returns a `SlopViolationError` forcing the LLM to provide a precise unified diff.

### Anti-Slop Full-Spectrum Matrix

The compile-time law is the common execution contract; focused skills supply the rules for each surface. The intended Context Compositor behavior is to bind skills by task, detected stack, file glob, and operation, rather than loading every rule into every prompt. On create/edit/patch/review operations, the daemon should supply applicable constraints before generation; validators and stream/tool gates should check the resulting output. Phase 2 covers the existing core law, stream filter, patch gate, and general verification loop; the surface-specific skill packs and checks listed as incomplete in Phase 3 remain roadmap work. Missing a skill must not be represented as successful validation.

| Dimension | Rules and anti-patterns | Auto-binding and enforcement |
|---|---|---|
| **Compile-time & Architectural Engine Law** | `ANTI_SLOP_LAW` is a top-priority invariant: concise signal, scoped changes, evidence-based verification, and no fabricated project details. | Compiled into the Rust core and injected in System Frame #0 for every task, including subagents. Tool permissions, patch validation, stream filtering, and verification gates enforce the applicable rules at runtime; prompt text alone is not treated as enforcement. |
| **Code Slop** | Avoid defensive over-engineering, unnecessary abstractions, speculative generalization, useless try/catch or wrapper layers, premature micro-optimization, redundant tests, and full-file re-emits. Keep comments informative rather than decorative or merely restating code. | Bind Code skills on relevant code operations: `.rs` (Rust), `.ts`/`.tsx` (TypeScript), and other registered source globs. `file_patch` favors minimal hunks and rejects avoidable whole-file replacement; select language-specific checks and report real linter/compiler output. |
| **UI/UX & Design Slop** | Reject generic purple-gradient AI templates, formulaic three-card hero landings, low-contrast text, unstyled controls, arbitrary generic animation, and desktop-only layouts. Require a deliberate visual hierarchy, usable states, accessible contrast, and a mobile layout. | Bind UI/design guidance when UI surfaces are created or changed, especially `.tsx`, `.jsx`, `.css`, and registered template/style files; mobile-layout guidance applies to responsive surfaces. Runtime review/check gates assess contrast, breakpoints, semantics, interaction states, and reduced-motion needs where tooling permits; do not claim visual checks that were not run. |
| **Copywriting & Prose Slop** | Remove buzzword-heavy corporate filler, throat-clearing, canned transitions, overused terms such as “delve,” “testament,” and “beacon,” emoji overload, and unnecessary apologies. Prefer concrete, direct, audience-appropriate language. | Bind Copywriting/Human-style skills to prose work and content-file edits such as `.md` and `.txt` (and UI copy embedded in components). A prose delivery gate checks specificity, relevance, tone, and unsupported claims; stream filtering may strip known prefatory filler without rewriting substantive user content. |
| **Structural & Agent Execution Slop** | No grading own homework, endless self-narration, ignored linter diagnostics, hallucinated imports/APIs, or claims unsupported by tool results. Keep edits within task scope and distinguish verified facts from assumptions. | The compositor applies this law to every agent and delegated task. Tool results are authoritative: verification consumes actual exit status/output, blocks or flags unresolved failures, and the final response states only checks performed. A patch/import review catches unsupported dependency changes where repository tooling allows. |

File extensions are routing hints, not the only trigger: task intent, workspace manifests, skill metadata, and explicitly requested operations can bind or suppress a skill. Multiple relevant dimensions may bind together (for example, a `.tsx` change with UI copy binds UI and prose guidance). Rules run before generation and at the appropriate stream, patch, and verification gates; an unavailable validator is disclosed, not simulated.

---

## 🌐 Provider Backends: OpenAI-Compatible Direct, or the OpenCode Gateway

By default `openchatd` talks OpenAI-compatible `/chat/completions` straight to any endpoint (`[llm] provider = "openai-compat"`). As an alternative backend, `[llm] provider = "opencode"` borrows a local, trusted `opencode serve` as a **model gateway**: its credential vault fronts OpenCode Zen (`:free` models) and OpenCode Go (30+ models, one login) with zero extra key management for the user. Catalog + completion run over the native v2 API (`/api/model`, `/api/experimental/generate/text`, HTTP Basic auth via `OPENCODE_PASSWORD`); the adapter is honestly **chat-only** (no tool definitions on that route), and the capability flag makes the agent loop drop tool declarations accordingly - no hallucinated calls, no faked enforcement. Verified live: 125 catalog models surfaced through the picker, 23 flagged free, a real Go-model turn closed the loop.

## 🛠️ OpenChamber Shortfalls & The `open-chat` Architectural Fixes

Through real-world development and multi-agent issue analysis in OpenChamber, eleven critical bottlenecks emerge. `open-chat` is architected from the ground up to eliminate every single one:

| # | OpenChamber Shortfall | What Goes Wrong | The `open-chat` Fix |
|---|---|---|---|
| **1** | **Multi-Agent Host Lockups (PC Freezes)** | Spawning 3+ concurrent agents creates unconstrained child processes, unbounded thread pools, multiple file watchers, and competing compiler checks that saturate 100% CPU/RAM and freeze the OS. | **Multi-Agent Resource Governor & User CPU Core Caps:** Hard concurrency semaphores, user-configured CPU core limits (`max_cpu_cores`), single shared workspace file-watcher, queue-managed build/compiler checks, and OS process priority throttling. **Changeable on the fly via command (`/cores <n>`) or CLI.** |
| **2** | **5× Output Token Bloat & Conversational Slop** | Full-file rewrites and conversational filler cause output tokens to exceed input tokens by 5×, triggering huge API bills and 60s+ latency. | **Anti-Slop Baked into Code as Inviolable Law:** Compiled into Rust binary, stream-level slop trimmer, and surgical search-and-replace tooling. |
| **3** | **Runtime Memory Bugs (Leaks, SIGKILL & Unbounded Buffers)** | Unbounded agent log buffers, unpruned tool execution history, and lack of system memory awareness trigger runtime OOM panics or process freezes. | **Self-Monitoring Memory Guardrails & Diagnostic Self-Healing:** Hard caps on execution trace buffers, active memory telemetry, and auto-compaction before memory pressure triggers crash events. |
| **4** | **Subagent Hangs & Deadlocks (Async Desync)** | Orchestrator-subagent runs hang indefinitely after 20–40 tool calls because parent sessions lose track of child SSE events or drop wake-ups. | **Strict Tokio Channel Ack & Timeout Deadlock Detector:** Typed async mpsc channels with heartbeat ping/pong, background task detached supervisors, and fail-safe timeouts. |
| **5** | **DOM Overload & Multi-Second Input Lag** | Long, tool-rich chats mount 1,000+ DOM nodes unconditionally outside the virtualizer (e.g. streaming tail turns), ballooning renderer RAM to >1.2GB and freezing typing. | **Aggressive Virtualization & Compact Headless JSON Feeds:** True virtualized viewport rendering; turns older than the immediate step render only collapsed summary envelopes. |
| **6** | **Context Window Opacity** | No visibility into prompt assembly; bloated MCP tool schemas and unpruned history quietly push models past 80k tokens into the "lost-in-the-middle" zone. | **Context Compositor & Token Badges:** Turn-by-turn inspector showing exact token counts for skills, tools, and history with deterministic budgeting. |
| **7** | **Transient Stream Resets Freeze Chat** | LLM provider timeouts or TCP socket resets leave the UI in a perpetual "spinning/stalled" state with ambiguous retry states. | **Resilient SSE Reconnection with Idempotency Keys:** Automatic reconnect exponential backoff with turn replay buffers so network blips never leave orphaned states. |
| **8** | **Electron Client Lock-in & Tab Fragility** | Engine tied directly to the frontend/browser session; tab closes or background memory kills crash ongoing runs. | **Headless Background Daemon:** Detached background daemon process accessible via Telegram, Discord, CLI, or lightweight Web UI. |
| **9** | **Cross-Session Amnesia** | Static `AGENTS.md` only; no persistent memory across chats, causing repeated questions and duplicate instructions. | **Dual-Tier Embedded Memory:** SQLite FTS5 (sparse) + `sqlite-vec` (dense) retaining workspace decisions and user habits permanently. |
| **10** | **All-or-Nothing Rule Ingestion** | Rules for all languages/tools injected simultaneously, confusing model attention and diluting focus. | **Autonomous Stack Auto-Binding:** Only loads matching skills for active files (e.g. Rust skills only when touching `.rs` files). |
| **11** | **Unverified Silent Edits** | Generates file diffs without executing project linters or compilers, leaving broken syntax for manual debugging. | **Autonomous Self-Healing Loop:** Automatically executes `cargo check` / `tsc` / `pytest` in background before closing the turn. |

---

## 🛑 Why 3 Concurrent Agents Lock Up a PC (and How `open-chat` Solves It)

When running multiple agents concurrently (e.g., 3 agents simultaneously running in OpenChamber), host computers freeze, fans ramp up to 100%, and mouse cursors stutter. This happens due to **four compounding system bottlenecks**:

```
[Agent 1: Code Gen]     [Agent 2: Test Runner]     [Agent 3: Linter/Indexer]
         │                         │                         │
         ├─────────────────────────┼─────────────────────────┤
         ▼                         ▼                         ▼
┌────────────────────────────────────────────────────────────────────────┐
│                   UNGOVERNED SYSTEM CONTENTION                         │
│  ❌ 3x Heavy Language Servers / File Watchers (Chokidar / Notify)       │
│  ❌ 3x Parallel Compiler Invocations (cargo check / tsc / bundlers)     │
│  ❌ 3x Unbounded Node.js / V8 Runtimes competing for memory & garbage  │
│  ❌ Disk I/O Saturation (locks on target/, node_modules/, SQLite WAL)   │
└────────────────────────────────────────────────────────────────────────┘
                                   │
                                   ▼
                 [🔥 100% CPU / RAM Saturation ➔ PC LOCKUP]
```

### The `open-chat` Multi-Agent Resource Governor with User CPU Throttling

`open-chat` introduces an explicit host resource arbiter built into its Rust daemon with **first-class user controls over CPU core allocation**:

```
[Agent 1]               [Agent 2]               [Agent 3]
    │                       │                       │
    ▼                       ▼                       ▼
┌───────────────────────────────────────────────────────────────┐
│              open-chat Host Resource Arbiter                  │
│                                                               │
│  1. User Core Allocation (max_cpu_cores / affinity mask)      │
│  2. Dynamic Slash Commands: `/cores <n>` & CLI adjustments    │
│  3. Concurrency Semaphore: Max parallel heavy tasks           │
│  4. Build/Lint Queue: Serialized `cargo check` / `tsc` runs    │
│  5. Shared Workspace Watcher: 1 single file debouncer for all │
│  6. Process Priority: Subprocess `nice` (Unix) / BelowNormal  │
│  7. Memory Headroom Guard: Auto-throttles if free RAM < 15%   │
└───────────────────────────────────────────────────────────────┘
                                │
                                ▼
         [🟢 Smooth, Responsive System • Zero OS Freezes]
```

### User Configuration: Explicit CPU Core & Concurrency Budget

Users can set exact limits in `.openchat/config.toml` or `~/.openchat/config.toml`:

```toml
[resources]
# Maximum CPU cores open-chat daemon and spawned tools may use (e.g., 2, 4, 6, or "50%")
# Default: leaves at least 2 cores completely free for your OS, browser, and IDE.
max_cpu_cores = 4

# Maximum concurrent compiler/build subprocesses across ALL agents
max_concurrent_builds = 1

# Maximum memory ceiling for open-chat workers (e.g., "4GB" or "40%")
max_worker_memory = "4GB"

# Automatically reduce subagent process priority (Unix nice / Windows BELOW_NORMAL)
low_process_priority = true
```

#### How Core Throttling Is Enforced:
1. **CPU Core Affinity Mask:** On Linux (`sched_setaffinity`) and Windows (`SetProcessAffinityMask`), the daemon binds tool child processes (compilers, linters, tests, Node/Cargo scripts) strictly to the designated CPU core mask (e.g., cores 0–3 of an 8-core CPU), physically preventing them from touching the remaining cores.
2. **Build Tool Thread Injection:** When spawning tools, `open-chat` automatically injects environment variables matching your core budget:
   - `RAYON_NUM_THREADS = max_cpu_cores`
   - `CARGO_BUILD_JOBS = max_cpu_cores`
   - `UV_THREAD_POOL_COUNT = max_cpu_cores`
   - `MAKEFLAGS = "-j{max_cpu_cores}"`
3. **Changeable on the Fly via Commands & Interactive Controls:**
   - **Chat Commands (Telegram, Discord, Web Chat):**
     - `/cores <n>` — Dynamically reallocates CPU core budget in real-time (e.g., `/cores 2` during a meeting, `/cores 8` for an intensive background run).
     - `/governor status` or `/cpu` — Displays active core allocations, live affinity masks, active worker pids, and process niceness deratings.
     - `/cores reset` — Resets allocations back to configuration file defaults.
   - **Terminal / CLI Command:**
     - `openchat governor set-cores <n>` — Reconfigures the running background daemon via local IPC socket without terminating running tasks or restarting the daemon.
   - **Web UI:** A sticky slider pill: `⚙️ CPU Cap: [ 4 / 8 Cores ]` allows quick throttling on the fly.

---

## 💡 The Core Philosophy: Why Skills Beat Deep-Layer Model Weights

Relying on an LLM's deep latent weights to "remember" idiomatic patterns, security invariants, and project conventions has three critical flaws:
1. **Latent "Drift" & Hallucination:** When an LLM has to dig through trillions of parameters across hundreds of programming paradigms, it defaults to the most generic, average internet code (slop, deprecated syntax, or bad habits).
2. **Attention & Context Penalty:** Shoving 50+ full skill files into the prompt wastes 25,000+ tokens and triggers context dilution (where the model ignores middle instructions).
3. **Cognitive Priming via Externalized Skills:** Modular skills act as **external cognitive anchors**. Feeding just 300–700 tokens of laser-focused constraints right into the top context window primes the attention mechanism directly. The LLM spends zero energy "wondering" what conventions to follow and generates exact, idiomatic code on the first attempt with significantly lower latency and token expenditure.

---

## 📉 Fixing the 5× Output Token Crisis: Surgical Diffs & Anti-Slop Law

In standard agent setups, output tokens run **5× higher than input tokens** because the agent rewrites entire 500-line files to change 5 lines of code, preceded and followed by polite conversational chatter. Because output tokens are **3× to 5× more expensive** and generated sequentially at 30–80 tokens/sec, this ruins latency and balloons cost.

`open-chat` enforces a strict **Low-Output High-Signal Protocol**:

```
                       User requests code change
                                   │
                                   ▼
        ┌────────────────────────────────────────────────────────┐
        │  Embedded Anti-Slop Law Invariant                      │
        │  • Prohibits: pleasantries, post-code summaries,       │
        │    rewriting unmodified functions, redundant tests.    │
        │  • Enforced by Rust daemon stream filter.             │
        └────────────────────────────────────────────────────────┘
                                   │
                                   ▼
        ┌────────────────────────────────────────────────────────┐
        │  Surgical Unified Diff / Replace Tooling               │
        │  • Agent emits ONLY: exact search & replace blocks     │
        │  • Full file rewrites rejected with SlopViolationError │
        └────────────────────────────────────────────────────────┘
                                   │
                                   ▼
        ┌────────────────────────────────────────────────────────┐
        │  Autonomous Verification Pass                          │
        │  • Executes local linter/compiler (cargo check / tsc)  │
        │  • If clean: yields concise 1-line confirmation        │
        └────────────────────────────────────────────────────────┘
```

### Result:
- **Output tokens drop by 75–85%**, cutting API costs significantly.
- **Turn latency drops from 60s+ to under 5s.**
- The next turn's input window stays compact, avoiding compounding context bloat.

---

## 🛡️ Runtime Memory Issues & Subagent Reliability

### 1. Agent Process Memory Leaks & OOM Crashes
- **The Problem:** In Node/Electron-based systems like OpenChamber, tool execution buffers, multi-megabyte terminal outputs, and ungarbage-collected MCP client streams accumulate in RAM until the OS issues a `SIGKILL (OOM)` or the process freezes.
- **The `open-chat` Fix:**
  - **Rust Native Daemon with Zero-Copy Streams:** Replaces bulky V8/JS heaps with lightweight native Rust memory management.
  - **Ring-Buffered Execution Logs:** Command and tool outputs are capped in bounded circular buffers (e.g. max 256KB retained in RAM per process run).
  - **Memory Pressure Telemetry:** If host RAM usage spikes >85%, the engine triggers an automatic WAL flush, runs SQLite memory vacuuming, and sheds non-essential vector caches.

### 2. Subagent Deadlocks & Orchestration Desync
- **The Problem:** In OpenChamber, delegating work to subagents frequently causes the parent session to hang indefinitely (OpenChamber issues #690, #869). When a subagent finishes, the SSE message or wake-up hook fails to notify the parent orchestrator, freezing the session.
- **The `open-chat` Fix:**
  - **Rust Tokio MPSC Orchestrator:** Subagents run under supervised async tasks with explicit bidirectional heartbeat checks.
  - **Task Result Watchdog:** If a subagent does not emit progress or its completion signal within an active timeout budget, the supervisor triggers an automatic diagnostic wake-up or graceful failure back to the parent instead of stalling forever.

### 3. UI DOM Overload & Input Lag in Long Sessions
- **The Problem:** In long sessions with 30+ tool calls, OpenChamber unconditionally mounts raw message DOM trees, causing 1.2GB+ browser memory bloat and typing lag (OpenChamber issue #2267).
- **The `open-chat` Fix:**
  - **Collapsed Envelope Summaries:** Past turns stream to clients as lightweight JSON summaries; detailed tool payloads are kept off the client DOM until explicitly toggled.

---

## 🧠 Architectural Pillar 1: Inbuilt Native Memory

Standard agent wrappers suffer from amnesia across sessions and unprincipled prompt bloat. `open-chat` introduces an embedded, dual-tier local memory architecture:

### 1. Dual-Tier Storage Engine
- **Structured & Keyword Search (FTS5):** SQLite FTS5 for exact keyword matching, project metadata, tool configurations, and architectural rules.
- **Dense Vector Search (`sqlite-vec` / LanceDB):** Embedded vector storage without external Docker requirements for semantic retrieval of project conventions, patterns, and past debugging logs.
- **Hybrid Retrieval (RRF / Cross-Encoder):** Reciprocal Rank Fusion merging dense and sparse results with a 500–1,000 token budget injected per turn.

### 2. Scoped Memory Tiers
1. **Global User Memory (`~/.openchat/memory.sqlite`):**
   - Personal preferences (formatting, tone, keybindings, style guidelines).
2. **Project Workspace Memory (`.openchat/memory.sqlite`):**
   - Tech stack details, project languages, architectural constraints, test commands, active project skills & triggers.
3. **Session Working Memory (RAM/WAL):**
   - Active scratchpad, task checkpoints, intermediate debugging traces.

### 3. Asynchronous Lifecycle & Memory Consolidation
- Background reflection pass executed at end-of-turn or on idle.
- Automatic contradiction resolution (e.g., state management refactors overwrite previous conventions rather than duplicating).
- Native agent tools for self-management:
  - `memory_store(category, key, value, scope)`
  - `memory_recall(query, scope)`
  - `memory_forget(key, scope)`

---

## ⚡ Architectural Pillar 2: 50+ Skill Scale — Autonomous Language & Stack Auto-Binding

When managing a library of **55+ skills**, manually calling them or dumping them into the prompt is unworkable. `open-chat` replaces manual prompting with an **Autonomous Stack Detector & Index Registry**:

```
                       User starts / opens workspace
                                     │
                                     ▼
        ┌────────────────────────────────────────────────────────┐
        │  Project Language & Stack Detector                     │
        │  • Detects files: Cargo.toml, package.json, go.mod, …   │
        │  • Detects languages: Rust, TypeScript, Python, etc.   │
        └────────────────────────────────────────────────────────┘
                                     │
                                     ▼
        ┌────────────────────────────────────────────────────────┐
        │  Skill Indexing & Metadata Registry                    │
        │  (Fast indexing of all 55+ installed skills)           │
        │  Matches: languages, file globs, dependencies, modes   │
        └────────────────────────────────────────────────────────┘
                                     │
              ┌──────────────────────┴──────────────────────┐
              │                                             │
      [Skill Matched]                                 [No Skill Found]
              │                                             │
              ▼                                             ▼
┌───────────────────────────────┐             ┌───────────────────────────────┐
│ Persistent Auto-Mount         │             │ Skill Gap Detector            │
│ • Mounts idiomatic skills     │             │ • Prompts user to add/author  │
│ • Injects Anti-Slop Law       │             │   new skill for this language │
│ • Emits visual badge          │             │ • Can bootstrap SKILL.md      │
└───────────────────────────────┘             └───────────────────────────────┘
```

### 1. Zero-Friction Skill Manifest (`SKILL.md` frontmatter)
Every skill in `.openchat/skills/` (or global `~/.openchat/skills/`) declares what it targets:

```yaml
---
id: rust-idioms-and-concurrency
name: Rust Idiomatic Patterns & Memory Safety
tags: [rust, cargo, safety, async]
triggers:
  languages: ["rust"]
  manifest_files: ["Cargo.toml", "Cargo.lock"]
  file_patterns: ["*.rs"]
modes:
  during:
    autoload: true
    description: Enforces error handling with Result/Option, eliminates unneeded clones, verifies Send/Sync.
---
```

### 2. Multi-Tier Resolution: Stack Detection + Semantic Fallback
1. **Deterministic Project Fingerprinting:**
   - Detects `Cargo.toml` or `main.rs` $\rightarrow$ automatically loads all Rust-tagged skills in `during` mode.
   - If a new language is introduced to the project (e.g. adding a Python CLI or TypeScript frontend to a Rust backend), the detector fires a `workspace:stack_expanded` event and immediately binds matching skills.
2. **FTS5 / Semantic Skill Router:**
   - For skills without exact file bindings, an internal index routes tasks via similarity (e.g., "optimize SQL query" fetches `postgres-tuning` even in a Rust repo).

### 3. Dynamic Runtime Language Expansion & "Skill Gap" Detection
What happens when you begin working in a language or framework that **has zero matching skills in your 55-skill catalog** (e.g., introducing Zig, Mojo, or Gleam)?

Instead of silently falling back to unguided, generic LLM generation, `open-chat` triggers an interactive **Skill Gap Prompt**:

1. **Detection:** Workspace watcher detects `.zig` files or `build.zig`, queries the 55-skill catalog, and reports zero matches for language `zig`.
2. **Interactive Notification & Action Request:**
   - **Web UI:** Displays an actionable pill banner:  
     `⚠️ No skill found for Zig. [Generate Zig Skill] [Import from Repo] [Skip]`
   - **Telegram / Discord:** Posts an inline query with action buttons:
     > 💡 *Detected new language **Zig** (`build.zig`), but no skill matches in your 55 installed skills. Would you like me to create a `zig-patterns` skill or import one?*  
     > `[⚡ Auto-Generate Skill] [📂 Browse Community] [🚫 Skip]`
3. **Agent Self-Bootstrapping (`skill_bootstrap`):**
   - If accepted, the agent scans official best practices, language idioms, and current project files to generate a tailored `.openchat/skills/zig-patterns/SKILL.md`.
   - The newly minted skill is immediately indexed, bound in `during` mode, and persisted to `.openchat/memory.sqlite`.

### 4. Observable Execution Badges (Eliminating the "Silent Injection" Problem)
You will never have to wonder if a skill is active. Every turn includes transparent visual confirmation:

```json
{
  "type": "agent:skills_composed",
  "project": "rust-crawler",
  "detected_stack": ["rust", "cargo"],
  "active_skills": [
    { "id": "rust-idioms", "mode": "during", "tokens": 420 },
    { "id": "anti-slop-law", "mode": "hardcoded_law", "tokens": 280 }
  ],
  "total_skill_overhead": "700 tokens",
  "output_mode": "surgical_diff",
  "memory_health": "optimal",
  "host_governor": { "active_workers": 3, "cpu_cores_capped": 4, "cpu_throttle": "normal" }
}
```

- **Web UI:** A sticky header bar with interactive pills:
  `🦀 Rust Stack Active • ⚖️ Anti-Slop Law • ⚙️ rust-idioms (420 tokens) • ✂️ Diff-Mode • 🟢 Memory Clean • 🖥️ 4 Cores Capped`
- **Telegram & Discord:** Clean, compact header tag before replies:
  `⚡ [Loaded: rust-idioms, Anti-Slop Law | Memory: Clean | Governor: 4 Cores Max]`

---

## 🎨 Architectural Pillar 3: Prompt & Context Compositor

Inspired by modern agent compositor architectures, `open-chat` uses a dedicated **Context Compositor** pipeline before any request reaches the model:

```
[Inviolable Anti-Slop Law] + [User Prompt] + [Project Memory] + [Auto-Discovered Skills]
                                    │
                                    ▼
                     ┌──────────────────────────────┐
                     │     Context Compositor       │
                     │  - Injects Law into Frame #0 │
                     │  - Deduplicates rules        │
                     │  - Enforces token budgets    │
                     │  - Prunes conflicting advice │
                     │  - Restricts diff output     │
                     └──────────────────────────────┘
                                    │
                                    ▼
                           Model Execution
```

By assembling only the pertinent rules dynamically:
- Token overhead drops by **85–90%** compared to monolithic prompts.
- Attention heads focus directly on relevant constraints rather than parsing noise.
- Output generation is faster, cheaper, and strictly compliant with project conventions.

---

## 🌐 Architectural Pillar 4: Multi-Client Core Daemon

Instead of managing native mobile applications, `open-chat` provides a unified headless daemon exposing:
- **Telegram Bot Bridge:** Complete agent execution and monitoring on mobile devices via native Telegram chat with inline action buttons.
- **Discord Bot Bridge:** Multi-channel or thread-based session management for collaborative or personal workflows.
- **Lightweight Web UI:** Clean, responsive workspace client for desktop browsers.

---

## 📦 Packaging & Release (v1.0.0)

Two supported ways to get the daemon, both built by CI rather than by hand:

| Artifact | How | Notes |
| --- | --- | --- |
| **AppImage** (x86_64) | `curl -LO …/releases/latest/download/open-chat-1.0.0-x86_64.AppImage && chmod +x` then `./open-chat-…AppImage serve` | single file, no install; delta-updatable via the published `.zsync` |
| **Nix flake** | `nix build .#openchatd`, `nix run .`, `nix develop` | x86_64-linux today; version comes from `[workspace.package]` |

`packaging/build-appimage.sh` is the one recipe, used locally and in CI, so the published image is never built differently from the tested one:

- **Pinned tooling** - `packaging/tools.lock` records appimagetool + type2-runtime versions and sha256; a checksum mismatch fails the build instead of shipping silently changed bytes (`scripts/refresh-tooling.sh --write` refreshes deliberately).
- **Portability floor** - `--max-glibc 2.35` compares the binary's highest `GLIBC_*` symbol against the stated minimum and refuses to package anything newer. This is why releases build on ubuntu-22.04: an image assembled on a rolling distro carries that distro's glibc requirement and dies on older ones. `ldd` output is printed so the host-provided libraries are on the record.
- **Runtime proof, not layout proof** - CI runs the built image and requires `/healthz`, `/status`, `/usage`, the web panel and `/models` to answer, then repeats the run inside a Debian stable container with no `/dev/fuse` (`APPIMAGE_EXTRACT_AND_RUN=1`), so "it packages" and "it serves on someone else's machine" are separate verified claims.
- **AppStream metadata** - validated in the build with severities parsed from the report. One warning is accepted on purpose: `cid-desktopapp-is-not-rdns`, because a reverse-DNS component id would require a desktop file named after it, and AppImages require the desktop basename to be the application name. appimagetool is passed `-n` because it cannot distinguish a warning from an error (appstreamcli exits 3 for both) and would abort the build on that style note.
- **TLS trust** - discovered the hard way in CI: reqwest builds a TLS root store whenever a client is *constructed*, even for a client that only talks to `http://127.0.0.1`, and fails if no CA bundle can be found. The flake therefore points the build and the installed binary at nixpkgs' bundle (`SSL_CERT_FILE`); the cargo/AppImage builds get it from the distro's `/etc/ssl/certs`. On a host with neither, set `SSL_CERT_FILE` - this is a runtime requirement of the daemon, not a packaging detail.
- **Sandbox scope** - `nix flake check` runs the whole suite except the watcher crate: its tests wait for real inotify events and the Nix sandbox delivers none (observed as three timeouts, everything else green), so the flake excludes that crate instead of claiming filesystem watching was proven where it could not be observed. The cargo CI job runs all 204 tests on a normal kernel.
- **`cargoHash`** - Nix pins the vendored crate graph by hash; it lives in `nix/cargoHash.txt` so a `Cargo.lock` change is a one-line diff, refreshed with `scripts/refresh-cargo-hash.sh --write` (reads the hash nix reports, never invents one).

The release workflow refuses to publish unless the pushed tag equals the version the built binary reports *and* the workspace manifest declares, so `v1.0.0`, `openchatd --version` and the metadata cannot disagree. (A first attempt read this from `cargo metadata --no-deps`, which reports `workspace_version: null` - caught before it could fail a real tag.) Branding remains interim: the icon is OpenChamber's logo under MIT with attribution in `crates/daemon/assets/NOTICE`, and the trademark caveat there still applies.

## 🗺️ Roadmap & Milestones

The `open-chat` implementation is structured into phased milestones addressing all core architecture layers, host resource governance, and OpenChamber fixes:

### Phase 1: Core Engine, Memory & Concurrency Foundation
- [x] **Rust Daemon Scaffolding:** Headless async Tokio core daemon decoupled from any browser or Electron process.
- [x] **Multi-Agent Host Resource Arbiter & CPU Governor:** Concurrency semaphores (`tokio::sync::Semaphore`), serialized build/lint check queue, and automated memory headroom guardrails.
- [x] **User-Configurable CPU Core Throttling:** Explicit user limit (`max_cpu_cores` via `.openchat/config.toml`, CLI, or chat command), OS core affinity pinning (`sched_setaffinity` on Linux / `SetProcessAffinityMask` on Windows), and compiler thread-cap injection (`CARGO_BUILD_JOBS`, `RAYON_NUM_THREADS`, `UV_THREAD_POOL_COUNT`, `MAKEFLAGS`).
- [x] **Changeable on the Fly via Commands:** Real-time throttling via interactive chat commands (`/cores <n>`, `/governor status`, `/cores reset`) and CLI command (`openchat governor set-cores <n>`) applied immediately via local daemon IPC without process restart.
- [x] **OS Process Niceness & Priority Manager:** Automatic scheduling prioritization (`nice` on Unix/macOS, `BELOW_NORMAL_PRIORITY_CLASS` on Windows) to prevent subagents from seizing user desktop interactivity.
- [x] **Shared Workspace File Watcher:** Single debounced file change bus replacing multiple uncoordinated file watchers per subagent.
- [x] **Bounded Memory Ring Buffers:** Zero-copy execution streams with strict circular buffer caps (256KB) preventing process RAM leaks and OOM kills.
- [x] **Subagent Task Supervisor & Watchdog:** Async channel orchestration with timeout safety nets and automatic wake-up triggers to eliminate subagent hangs.
- [x] **Dual-Tier Embedded Memory:** SQLite FTS5 (sparse exact) + `sqlite-vec` (dense semantic) with automatic contradiction resolution and WAL hygiene.
- [x] **Dynamic Rolling Context Compactor:** Automated pruning of intermediate terminal logs and tool payloads into semantic one-line digests.

### Phase 2: Token Efficiency, Core Engine Law & Runtime Anti-Slop Gates
- [x] **Compile-Time Anti-Slop Law Invariant:** Hardcoded `const ANTI_SLOP_LAW` embedded in Rust binary, injected into System Frame #0 above all prompt contexts.
- [x] **Stream Slop Interceptor:** In-flight Trie-based prefix filter that strips conversational pleasantries before reaching the user UI.
- [x] **Surgical Diff & Replace Engine:** Native `file_patch` / search-and-replace tool rejecting full-file rewrites with `SlopViolationError`.
- [x] **Autonomous Lint & Verification Loop:** Pre-completion check runner (`cargo check`, `tsc`, `pytest`) preventing unverified broken diffs from landing.
- [x] **Full-Spectrum Anti-Slop Contract:** Compile-time architectural law spans code, UI/design, copy/prose, and agent execution; relevant rules bind to the active task and file operations.
- [x] **Runtime Slop Gates:** Stream-level conversational filtering, surgical patch validation, and evidence-based verification; disclose missing/unavailable validators rather than claiming success.
- [x] **Code-Surface Anti-Slop Skill Pack:** Auto-bind code/comment hygiene by source globs; block needless abstractions, wrappers, catches, micro-optimizations, and full-file re-emits.
- [x] **UI/Design & Mobile Quality Skill Pack:** Auto-bind design, accessibility/contrast, interaction-state, and responsive/mobile checks for UI surfaces.
- [x] **Copywriting/Human Prose Skill Pack:** Auto-bind direct-language guidance to `.md`, `.txt`, and UI copy; screen for filler, buzzwords, canned phrasing, and emoji overload.
- [x] **Runtime Code Memory Bug Diagnostics:** Automated memory-safety skill injection (RAII, weak refs, pointer hygiene) and diagnostic trap on memory sanitizer errors.

### Phase 3: 50+ Skill Scale, Full-Spectrum Anti-Slop & Stack Discovery
- [x] **Deterministic Stack Fingerprinter:** Real-time workspace scanner detecting active languages (`Cargo.toml`, `package.json`, `go.mod`, etc.).
- [x] **55+ Skill Index Registry:** YAML frontmatter parser and indexing engine mapping skills to languages, globs, dependencies, and execution modes.
- [x] **Full-Spectrum Anti-Slop Skill Suite:** Integrate Code, UI/Design/Mobile, Copywriting/Human, and core Anti-Slop skills from the [reference repository](https://github.com/miqdadbadjuber/anti-slop); preserve scoped rules and provenance.
- [x] **Operation-Aware Auto-Binding:** Bind UI rules for `.tsx`/`.css`, Code rules for `.rs`/`.ts`, and Copywriting rules for `.md`/`.txt` create/edit/review operations, with intent- and manifest-based routing beyond extensions.
- [x] **Runtime Stream & Surface Filtering:** Enforce output/prose gates and patch-time code/UI checks, then run available formatters, linters, accessibility/contrast and responsive checks. Record actual results and disclose unavailable checks.
- [x] **Skill Gap Detector & Synthesizer:** Interactive prompt flow alerting users on uncovered stacks or content surfaces with one-click `skill_bootstrap` authoring.

### Phase 4: Context Compositor & Observability
- [x] **Context Compositor Pipeline:** Pre-flight prompt assembler that injects Anti-Slop Law, deduplicates rules, and enforces strict token budgets.
- [x] **Real-Time Token, Memory & Governor Badges:** Transparent per-turn telemetry showing active skills, token consumption, memory health status, active worker count, and CPU core cap.

### Phase 5: Multi-Interface Adapters & Resilient UI
- [x] **Resilient SSE Reconnection with Idempotency:** Auto-reconnecting event streams with replay buffers preventing UI hangs on provider timeouts.
- [x] **Lightweight Virtualized Web UI:** Collapsed envelope rendering preventing 1.2GB+ DOM overload and input stutter in long sessions.
- [x] **Interactive Dynamic Core Throttling Controls:** OpenChamber-style header pill / settings slider and bot chat commands (`/cores <n>`, `/governor status`) for real-time CPU throttling adjustments via daemon IPC.
- [x] **Telegram Bot Gateway:** Mobile control plane with inline action buttons for skill gap prompts, diff reviews, core throttling, and execution status.
- [ ] **Discord Bot Gateway:** Thread-based workspace session manager for team and multi-project workflows.
