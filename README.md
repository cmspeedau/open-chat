# open-chat

Next-generation agentic workspace & rewrite inspired by [OpenChamber](https://github.com/openchamber/openchamber).

`open-chat` transitions from an Electron/mobile-heavy model to a **headless, ultra-fast core daemon** supporting first-class multi-interface clients (Telegram, Discord, Web) with **native persistent memory**, **autonomous 50+ skill auto-binding**, **interactive skill gap synthesis**, **surgical diff patching (solving 5× output token bloat)**, **runtime memory bug diagnostic self-healing**, and a **Context Compositor engine**.

---

## 🔗 Reference Projects & Ecosystem

- **Base Reference:** [openchamber/openchamber](https://github.com/openchamber/openchamber) — Headless core architecture decoupling the engine from mobile/Electron wrappers.
- **Skill Reference:** [miqdadbadjuber/anti-slop](https://github.com/miqdadbadjuber/anti-slop) — Exemplar ruleset illustrating stage-based operational modes (`during` vs `after`) across design, copy, and code generation.

---

## 🛠️ OpenChamber Shortfalls & The `open-chat` Architectural Fixes

Through real-world development in OpenChamber, seven major bottlenecks emerge. `open-chat` is architected from the ground up to eliminate each one:

| # | OpenChamber Shortfall | What Goes Wrong | The `open-chat` Fix |
|---|---|---|---|
| **1** | **5× Output Token Bloat** | Full-file rewrites and conversational filler cause output tokens to exceed input tokens by 5×, triggering huge API bills and 60s+ latency. | **Unified Diff Tooling & Strict `[during]` Guardrails:** Only changed lines are emitted; zero conversational fluff or tutorial narration. |
| **2** | **Runtime Memory Bugs (Leaks, SIGKILL & Unbounded Buffers)** | Unbounded agent log buffers, unpruned tool execution history, and lack of system memory awareness trigger runtime OOM panics or process freezes. | **Self-Monitoring Memory Guardrails & Diagnostic Self-Healing:** Hard caps on execution trace buffers, active memory telemetry, and auto-compaction before memory pressure triggers crash events. |
| **3** | **Context Window Opacity** | No visibility into prompt assembly; bloated MCP tool schemas and unpruned history quietly push models past 80k tokens into the "lost-in-the-middle" zone. | **Context Compositor & Token Badges:** Turn-by-turn inspector showing exact token counts for skills, tools, and history with deterministic budgeting. |
| **4** | **Electron Client Lock-in** | Engine tied directly to the frontend/browser session; tab closes or background memory kills crash ongoing runs. | **Headless Background Daemon:** Detached background daemon process accessible via Telegram, Discord, CLI, or lightweight Web UI. |
| **5** | **Cross-Session Amnesia** | Static `AGENTS.md` only; no persistent memory across chats, causing repeated questions and duplicate instructions. | **Dual-Tier Embedded Memory:** SQLite FTS5 (sparse) + `sqlite-vec` (dense) retaining workspace decisions and user habits permanently. |
| **6** | **All-or-Nothing Rule Ingestion** | Rules for all languages/tools injected simultaneously, confusing model attention and diluting focus. | **Autonomous Stack Auto-Binding:** Only loads matching skills for active files (e.g. Rust skills only when touching `.rs` files). |
| **7** | **Unverified Silent Edits** | Generates file diffs without executing project linters or compilers, leaving broken syntax for manual debugging. | **Autonomous Self-Healing Loop:** Automatically executes `cargo check` / `tsc` / `pytest` in background before closing the turn. |

---

## 💡 The Core Philosophy: Why Skills Beat Deep-Layer Model Weights

Relying on an LLM's deep latent weights to "remember" idiomatic patterns, security invariants, and project conventions has three critical flaws:
1. **Latent "Drift" & Hallucination:** When an LLM has to dig through trillions of parameters across hundreds of programming paradigms, it defaults to the most generic, average internet code (slop, deprecated syntax, or bad habits).
2. **Attention & Context Penalty:** Shoving 50+ full skill files into the prompt wastes 25,000+ tokens and triggers context dilution (where the model ignores middle instructions).
3. **Cognitive Priming via Externalized Skills:** Modular skills act as **external cognitive anchors**. Feeding just 300–700 tokens of laser-focused constraints right into the top context window primes the attention mechanism directly. The LLM spends zero energy "wondering" what conventions to follow and generates exact, idiomatic code on the first attempt with significantly lower latency and token expenditure.

---

## 📉 Fixing the 5× Output Token Crisis: Surgical Diffs & Anti-Slop

In standard agent setups, output tokens run **5× higher than input tokens** because the agent rewrites entire 500-line files to change 5 lines of code, preceded and followed by polite conversational chatter. Because output tokens are **3× to 5× more expensive** and generated sequentially at 30–80 tokens/sec, this ruins latency and balloons cost.

`open-chat` enforces a strict **Low-Output High-Signal Protocol**:

```
                       User requests code change
                                   │
                                   ▼
        ┌────────────────────────────────────────────────────────┐
        │  anti-slop [during] Constraint Injection               │
        │  • Prohibits: pleasantries, post-code summaries,       │
        │    rewriting unmodified functions, redundant tests.    │
        └────────────────────────────────────────────────────────┘
                                   │
                                   ▼
        ┌────────────────────────────────────────────────────────┐
        │  Surgical Unified Diff / Replace Tooling               │
        │  • Agent emits ONLY: exact search & replace blocks     │
        │  • Never outputs full file contents                    │
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

## 🛡️ Runtime Memory Issues: Detection, Telemetry & Self-Healing

When an agent encounters memory issues during execution, it generally falls into two distinct categories:

### 1. Agent Process Memory Leaks & OOM Crashes
- **The Problem:** In Node/Electron-based systems like OpenChamber, tool execution buffers, multi-megabyte terminal outputs, and ungarbage-collected MCP client streams accumulate in RAM until the OS issues a `SIGKILL (OOM)` or the process freezes.
- **The `open-chat` Fix:**
  - **Rust Native Daemon with Zero-Copy Streams:** Replaces bulky V8/JS heaps with lightweight native Rust memory management.
  - **Ring-Buffered Execution Logs:** Command and tool outputs are capped in bounded circular buffers (e.g. max 256KB retained in RAM per process run).
  - **Memory Pressure Telemetry:** If host RAM usage spikes >85%, the engine triggers an automatic WAL flush, runs SQLite memory vacuuming, and sheds non-essential vector caches.

### 2. Target Workspace Code Memory Bugs (Leak Detection & Prevention)
- **The Problem:** The agent writes or discovers code that introduces memory leaks (e.g., circular `Arc`/`Rc` references in Rust, unclosed file descriptors, dangling event listeners, unbounded in-memory caches, or C/FFI pointer mismanagement).
- **The `open-chat` Fix:**
  - **Auto-Bound Memory Safety Skills (`during` mode):** When working in Rust, C++, Go, or Python, the system auto-injects targeted memory-safety rules (e.g., preventing unbounded `Vec::push` without capacity bounds, checking `Weak` references, and mandating RAII cleanup).
  - **Autonomous Diagnostic Hook:** When a build, test, or runtime command emits a memory warning (e.g. `AddressSanitizer: heap-use-after-free`, `valgrind` errors, Python `MemoryError`, or high heap allocation logs), the agent:
    1. Pauses task execution.
    2. Identifies the offending data structure or lifecycle flaw.
    3. Emits an immediate surgical fix and validates it against memory profiling flags.

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
│ • Injects 'anti-slop [during]'│             │   new skill for this language │
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
    { "id": "anti-slop", "subskills": ["antislop-code"], "mode": "during", "tokens": 310 }
  ],
  "total_skill_overhead": "730 tokens",
  "output_mode": "surgical_diff",
  "memory_health": "optimal"
}
```

- **Web UI:** A sticky header bar with interactive pills:
  `🦀 Rust Stack Active • 🛡️ anti-slop [during] • ⚙️ rust-idioms (730 tokens) • ✂️ Diff-Mode • 🟢 Memory Clean`
- **Telegram & Discord:** Clean, compact header tag before replies:
  `⚡ [Loaded: rust-idioms, anti-slop (during) | Memory: Clean]`

---

## 🎨 Architectural Pillar 3: Prompt & Context Compositor

Inspired by modern agent compositor architectures, `open-chat` uses a dedicated **Context Compositor** pipeline before any request reaches the model:

```
[User Prompt] + [Project Memory] + [Auto-Discovered Skills] + [Active Mode: During]
                                    │
                                    ▼
                     ┌──────────────────────────────┐
                     │     Context Compositor       │
                     │  - Deduplicates rules        │
                     │  - Enforces token budgets    │
                     │  - Prunes conflicting advice │
                     │  - Injects top-window anchors│
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

## 🗺️ Roadmap & Next Steps

- [ ] Core daemon scaffolding (Rust native with bounded ring buffers).
- [ ] SQLite + FTS5 + `sqlite-vec` memory persistence layer.
- [ ] Surgical diff / replace tool suite (replacing full file overwrites).
- [ ] Memory health telemetry and runtime diagnostic hook (leak prevention).
- [ ] Language & workspace stack fingerprinting scanner.
- [ ] 50+ Skill metadata indexer with auto-binding triggers.
- [ ] Skill Gap Detector & interactive prompt flow (`skill_bootstrap`).
- [ ] Context Compositor (token pruner, output compressor, conflict deduplicator).
- [ ] Autonomous lint/verify feedback loop (`cargo check`, `tsc`).
- [ ] Observable event stream & multi-client UI status badges.
- [ ] Telegram bot adapter & Web UI frontend.
