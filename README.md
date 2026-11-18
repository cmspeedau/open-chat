# open-chat

Next-generation agentic workspace & rewrite inspired by [OpenChamber](https://github.com/openchamber/openchamber).

`open-chat` transitions from an Electron/mobile-heavy model to a **headless, ultra-fast core daemon** supporting first-class multi-interface clients (Telegram, Discord, Web) with **native persistent memory**, **autonomous 50+ skill auto-binding**, and a **Context Compositor engine**.

---

## 🔗 Reference Projects & Ecosystem

- **Base Reference:** [openchamber/openchamber](https://github.com/openchamber/openchamber) — Headless core architecture decoupling the engine from mobile/Electron wrappers.
- **Skill Reference:** [miqdadbadjuber/anti-slop](https://github.com/miqdadbadjuber/anti-slop) — Exemplar ruleset illustrating stage-based operational modes (`during` vs `after`) across design, copy, and code generation.

---

## 💡 The Core Philosophy: Why Skills Beat Deep-Layer Model Weights

Relying on an LLM's deep latent weights to "remember" idiomatic patterns, security invariants, and project conventions has three critical flaws:
1. **Latent "Drift" & Hallucination:** When an LLM has to dig through trillions of parameters across hundreds of programming paradigms, it defaults to the most generic, average internet code (slop, deprecated syntax, or bad habits).
2. **Attention & Context Penalty:** Shoving 50+ full skill files into the prompt wastes 25,000+ tokens and triggers context dilution (where the model ignores middle instructions).
3. **Cognitive Priming via Externalized Skills:** Modular skills act as **external cognitive anchors**. Feeding just 300–700 tokens of laser-focused constraints right into the top context window primes the attention mechanism directly. The LLM spends zero energy "wondering" what conventions to follow and generates exact, idiomatic code on the first attempt with significantly lower latency and token expenditure.

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
                      Language/Context Matched
                                     │
                                     ▼
        ┌────────────────────────────────────────────────────────┐
        │  Persistent Auto-Mount in Project Memory               │
        │  • E.g. Rust detected ➔ mounts 'rust-patterns',        │
        │    'anti-slop [during]', 'cargo-analyzer'              │
        │  • Keeps other 52 skills dormant in the index          │
        │  • Emits visual badge & logs exact token budget        │
        └────────────────────────────────────────────────────────┘
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

### 3. Dynamic Runtime Language Expansion
When a new file type or language is created:
1. Agent generates or touches `src/lib.rs` or runs `cargo init`.
2. Workspace watcher flags `new_language_detected: rust`.
3. The daemon scans all 55+ skills, identifies all matching skills (`rust-patterns`, `clippy-enforcer`, `anti-slop`), and activates them immediately without interrupting the flow.

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
  "total_skill_overhead": "730 tokens"
}
```

- **Web UI:** A sticky header bar with interactive pills:
  `🦀 Rust Stack Active • 🛡️ anti-slop [during] • ⚙️ rust-idioms (730 tokens)`
- **Telegram & Discord:** Clean, compact header tag before replies:
  `⚡ [Loaded: rust-idioms, anti-slop (during)]`

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

- [ ] Core daemon scaffolding (Rust).
- [ ] SQLite + FTS5 + `sqlite-vec` memory persistence layer.
- [ ] Language & workspace stack fingerprinting scanner.
- [ ] 50+ Skill metadata indexer with auto-binding triggers.
- [ ] Context Compositor (token pruner & conflict deduplicator).
- [ ] Observable event stream & multi-client UI status badges.
- [ ] Telegram bot adapter & Web UI frontend.
