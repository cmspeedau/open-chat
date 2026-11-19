# OpenChat / OpenChamber Rewrite — Architecture & Product Ideas

> **Status:** consolidated design note and implementation proposal. The ideas below combine the supplied OpenChat/OpenChamber notes with the requested three-layer anti-slop architecture, Dev Mode workflow, small-model bootstrap/diagnostics engine, multi-client / directory-scoped synchronization architecture, native REA (Reverse Engineer Anything) reconstruction capabilities, and an integrated Clean-Room vs. Deep Mode toggle switch with automated tool installation grounded in the `craft-anything` architecture. Names of upstream projects are attribution/reference points, not claims of integration or endorsement.
>
> **Design principle:** generate economically, constrain at inference where possible, inspect changed syntax before it reaches disk, keep every automated diagnosis or external action observable and user-controlled, offload operational triage/bootstrap tasks to lightweight local models, enforce strict state-sync invariants across concurrent multi-client sessions, integrate native binary/app reverse-engineering pipelines directly into the developer workflow, and simplify execution via a single primary toggle: **Clean-Room vs. Deep Mode** paired with an automated dependency installer and `doctor` system.

## Contents

1. [Executive overview](#1-executive-overview)
2. [Lightweight local model bootstrap & onboarding](#2-lightweight-local-model-bootstrap--onboarding)
3. [Autonomous skill synthesis pipeline](#3-autonomous-skill-synthesis-pipeline)
4. [Inbuilt REA Subsystem & Craft-Anything Architecture](#4-inbuilt-rea-subsystem--craft-anything-architecture)
5. [The Clean-Room vs. Deep Mode Toggle & Tool Installer](#5-the-clean-room-vs-deep-mode-toggle--tool-installer)
6. [Polyglot anti-slop architecture](#6-polyglot-anti-slop-architecture)
7. [Multi-client sync protocol & state invariants (OpenChamber-grounded)](#7-multi-client-sync-protocol--state-invariants-openchamber-grounded)
8. [Unified orchestration lifecycle](#8-unified-orchestration-lifecycle)
9. [Interfaces and implementation sketches](#9-interfaces-and-implementation-sketches)
10. [OpenChat Dev Mode & small-model diagnostics offload](#10-openchat-dev-mode--small-model-diagnostics-offload)
11. [OpenChat / OpenChamber rewrite direction](#11-openchat--openchamber-rewrite-direction)
12. [Roadmap and integration sequence](#12-roadmap-and-integration-sequence)
13. [Safety, failure modes, and design decisions](#13-safety-failure-modes-and-design-decisions)
14. [References and provenance](#14-references-and-provenance)

---

## 1. Executive overview

The proposal combines five complementary architectural pillars: a lightweight zero-cloud onboarding concierge, an inbuilt reverse-engineering analysis subsystem (REA) centered on a simple **Clean-Room vs. Deep Mode** toggle switch and tool installer manager patterned after `craft-anything`, followed by three runtime anti-slop quality controls:

| Layer / Stage | When it acts | Primary responsibility | Reference concept |
|---|---|---|---|
| **0. Local Bootstrap Coprocessor & Diagnostic Engine** | First run, workspace open, failure/incident triage | Hardware discovery, credential onboarding, local skill synthesis, log clustering & diagnostic drafting | NousResearch `hermes-agent` (`setup_beat` / local flows) |
| **0.5 Clean-Room vs. Deep Mode REA Engine & Tool Installer** | Binary inspection, clean-room feature reconstruction, settings management | Single toggle: outward clean-room recreation vs. deep decompilation; manage toolchains via integrated `doctor` | `morluto/rea` & `storytold/craft-anything` |
| **1. Prompt Skill Injector** | Before generation and after a patch | Select project/language rules, explain the required lifecycle mode, enforce compact search/replace or unified-diff output | `miqdadbadjuber/anti-slop`-style skill and `during` / `after` rules |
| **2. Logit Backtracking Sampler** | Inside token generation | Detect disallowed phrase continuations and retry from a cached prefix with adjusted probabilities | `sam-paech/antislop-sampler` |
| **3. Polyglot Tree-Sitter AST Gate** | After patch construction, before file write | Inspect syntax nodes for incomplete or suspicious constructs in supported languages | `skew202/antislop` / tree-sitter-based checks |

The controls are deliberately layered:

- **The bootstrap and diagnostic coprocessor** runs entirely on a lightweight local model (1B–3B parameters or embedded runtime), eliminating cold-start API dependencies, isolating dirty stack traces from the main context, and saving cloud token quotas during setup, onboarding, and incident triage.
- **Inbuilt REA capabilities (Craft-Anything style)** allow developers to point OpenChat at any website, desktop app, mobile APK, game, or native binary, study it with real inspection tools, build a new version in the user\'s requested language/stack, and verify the result against the original.
- **Clean-Room vs. Deep Mode Switch:** Eliminates complex UI toggles in favor of one clear operational switch:
  - **Clean-Room Mode (Default):** Never reads inside proprietary code, binaries, or private repo files. Works strictly from outward observable behavior, UI screenshots at multiple viewports, public network contracts, and specs.
  - **Deep Mode (Opt-In):** Unlocks the full 300+ decompiler and disassembler suite (headless Ghidra, JADX, ASAR unpacker, IL2CPP dumper, AST decoders).
  - Missing dependencies are detected and installed via an integrated `doctor` command and settings UI.
- **Prompt rules** guide intent and reduce unnecessary output, but remain probabilistic.
- **A sampler** can suppress selected language patterns only when the inference runtime exposes token-level logits/state; ordinary hosted chat APIs may not support it.
- **An AST gate** is deterministic over the parsed patch and can block known incomplete constructs, but is not a proof of correctness and should avoid policing ordinary application semantics as if they were universal truths.
- **Patch application** is transactional: validate paths and base versions, apply only the requested change, run syntax/hygiene checks, then commit to the workspace or return a structured rejection for repair.
- **State synchronization** guarantees multi-client convergence (CLI, Web, Telegram, Discord) with directory-scoped authority, optimistic update shadows, and monotonic write revisions.

---

## 2. Lightweight local model bootstrap & onboarding

### 2.1 The cold-start problem

Traditional coding agents rely on frontier cloud models (e.g. Claude 3.5 Sonnet, GPT-4o) from the very first interaction. This creates severe inefficiencies and bootstrapping friction:
1. **Unconfigured credentials:** When a user first installs OpenChat, no API keys or endpoints are configured. A frontier model cannot guide onboarding because it is not authenticated yet.
2. **Cold-start token & latency penalty:** Running multi-thousand-token system prompts and MCP tool definitions through heavy local models (32B/70B) or cloud APIs just to ask *"What is your Git username?"* or *"Select your default workspace"* wastes time and money.
3. **Fragile static wizards vs. conversational agility:** Hardcoded CLI configuration wizards are rigid and fail to adapt when host environments (GPU drivers, ollama ports, tool paths) deviate from standard setups.

### 2.2 The two-tier agent model (Hermes-inspired pattern)

OpenChat adopts an explicit two-tier model separation:

```text
┌────────────────────────────────────────────────────────────────────────┐
│             Tier 0: Local Bootstrap & Coprocessor (Small Model)        │
│                                                                        │
│  [Embedded / Fast Local Model: 1B–3B quantized (e.g. Qwen2.5-Coder)]   │
│  • Hardware & GPU/VRAM capability probe                                │
│  • Zero-cloud interactive onboarding cards (`setup_beat`)              │
│  • Local credential & provider setup (Ollama, vLLM, Cloud API keys)    │
│  • Workspace manifest detection & AST skill synthesis                  │
│  • Dev Mode diagnostic triage, trace redaction & issue drafting        │
│  • Mode prerequisite scanning & installer coordination (`doctor`)      │
└───────────────────────────────────┬────────────────────────────────────┘
                                    │ Bootstrap Complete / Hand-off
                                    ▼
┌────────────────────────────────────────────────────────────────────────┐
│                      Tier 1: Heavy Execution Engine                    │
│                                                                        │
│  [Cloud API (Claude 3.5, GPT-4o) OR Heavy Local Model (32B / 70B)]     │
│  • Primary conversational coding & complex reasoning                   │
│  • Layer 1: Prompt Skill Injector                                      │
│  • Layer 2: Backtracking Sampler                                       │
│  • Layer 3: Polyglot Tree-Sitter AST Diff Gate                         │
│  • Headless Daemon Task Runner & Tool Executor                         │
│  • Clean-Room / Deep Mode Subagent Orchestrator                        │
└────────────────────────────────────────────────────────────────────────┘
```

### 2.3 Onboarding beats and interactive UI cards

Modeled after the `setup_beat` pattern in `hermes-agent`, onboarding in OpenChat is driven through sequential, deterministic **beats** executed by the bootstrap model:

1. **`hardware` beat:** Probes local compute resources (available RAM, VRAM, Apple Metal / CUDA presence) and recommends local model sizing or cloud gateway usage.
2. **`connectors` beat:** Fingerprints local developer utilities (Git, GitHub CLI `gh`, Docker, Ripgrep, Tree-sitter parsers) and displays interactive cards to enable or link them.
3. **`credentials` beat:** Securely collects and encrypts model provider keys or local endpoint ports directly into the daemon\'s local store.
4. **`workspace` beat:** Discovers project manifests (`Cargo.toml`, `pyproject.toml`, `package.json`, `go.mod`) in target folders and seeds project memory.
5. **`mode_setup` beat (Optional):** Prompts user for primary default mode (**Clean-Room** vs. **Deep**); runs `doctor` check for required tools and offers 1-click install.
6. **`done` beat:** Transitions the session from onboarding mode to the standard interactive coding daemon.

Crucially, cards are UI-agnostic: the daemon emits structured card payloads over WebSocket or IPC, allowing Web, TUI, and desktop frontends to render native pickers and skip controls.

---

## 3. Autonomous skill synthesis pipeline

### 3.1 Avoiding documentation scraping slop

When an agent needs a skill for a newly detected language or framework, standard practice has been to scrape official documentation sites or search results. This produces low-quality "prompt slop":
- **Documentation sites prioritize beginners:** Tutorials contain hundreds of tokens of introductory marketing prose and toy `hello world` snippets.
- **Omission of production invariants:** Beginner documentation frequently relies on simplified, sloppy idioms (e.g., blanket `.unwrap()` in Rust or bare `except:` in Python) for brevity.
- **Context bloat:** Feeding hundreds of lines of documentation markdown into prompt context dilutes the model\'s focus on project-specific architecture.

### 3.2 The repository-first synthesis engine

OpenChat synthesizes new skills by grounding itself **in the local project\'s ground truth first**, using compiler signatures and symbol dumps rather than web tutorials:

```text
                      Trigger: Unindexed Stack Detected
                                       │
                                       ▼
                  ┌─────────────────────────────────────────┐
                  │ 1. Manifest & Lockfile Inspection       │
                  │    • Exact framework & crate/pkg versions│
                  │    • Rust edition, Node runtime target  │
                  └────────────────────┬────────────────────┘
                                       │
                                       ▼
                  ┌─────────────────────────────────────────┐
                  │ 2. AST Grounding & Convention Sampling  │
                  │    • Sample 2-3 existing project files  │
                  │    • Extract error handling pattern     │
                  │      (e.g. thiserror vs anyhow)         │
                  │    • Extract concurrency conventions    │
                  └────────────────────┬────────────────────┘
                                       │
                                       ▼
                  ┌─────────────────────────────────────────┐
                  │ 3. Compiler Symbol & Signature Query    │
                  │    • Query local compiler/cargo doc     │
                  │    • Read exact type signatures offline │
                  └────────────────────┬────────────────────┘
                                       │
                                       ▼
                  ┌─────────────────────────────────────────┐
                  │ 4. Compact SKILL.md Compilation         │
                  │    • 30-50 line operational contract    │
                  │    • `during` rules (diff constraints)  │
                  │    • `after` rules (verification checks)│
                  │    • AST Gate blockers (todo, unwrap)   │
                  └─────────────────────────────────────────┘
```

The synthesized skill is stored in `crates/memory` (via SQLite/sqlite-vec) and injected on demand whenever the agent edits matching file patterns.

---

## 4. Inbuilt REA Subsystem & Craft-Anything Architecture

### 4.1 Product concept: Study → Build → Check → Ready

Adopting the architectural model established by `storytold/craft-anything` (and enriched by `morluto/rea`), OpenChat implements a full-lifecycle reconstruction engine that turns black-box software into clean-room, verified implementations:

```text
┌──────────────┐     ┌──────────────┐     ┌──────────────┐     ┌──────────────┐
│   1. STUDY   │ ──► │   2. BUILD   │ ──► │   3. CHECK   │ ──► │   4. READY   │
│ Real tools   │     │ Coding agent │     │ Compare with │     │ Open, test,  │
│ examine the  │     │ generates    │     │ original;    │     │ play or      │
│ target & log │     │ clean-room   │     │ score visual │     │ refine with  │
│ evidence     │     │ codebase     │     │ & logic diff │     │ conversation │
└──────────────┘     └──────────────┘     └──────────────┘     └──────────────┘
```

1. **Study:** Inspects the target using tools authorized under the active mode (**Clean-Room** or **Deep**). Gathers layout across breakpoints, API routes, or decompiled symbols, recording findings in a structured evidence ledger (`investigation.cany/`).
2. **Build:** A coding agent (Claude Code, Grok, Copilot, or local OpenChat executor via ACP/MCP) synthesizes an entirely new project in the stack requested by the user (e.g. Rust, TypeScript, Godot, React).
3. **Check:** Compares the build against the original target. For websites: responsive DOM and screenshot diffs at 3 viewport widths. For apps/games: execution runs, state machine transitions, and recorded test vectors.
4. **Ready:** Interactive launch, live preview, or play-test card in the client, with an iterative prompt box for subsequent refinements.

### 4.2 Target Analysis Matrix (`crates/tools::rea`)

Like `craft-anything`\'s 320-tool suite, OpenChat provides dual-mode tool implementations where every tool functions as both a daemon MCP tool and a standalone CLI subcommand:

1. **Websites & Web Apps:**
   - Playwright/headless Chrome capture at 3 screen widths (desktop 1440px, tablet 768px, mobile 375px).
   - Layout metrics, color palette, typography extraction, and public network call capture.
2. **Native Binaries (x86_64, AArch64, ARM, RISC-V):**
   - Headless bridge to Ghidra, Hopper, IDA, or Kuna decompilers (Deep Mode).
   - Built-in pure-Rust parsers (`goblin`, `object`) for ELF sections, PE imports, and Mach-O signatures without external decompiler overhead.
3. **JavaScript & Electron Applications:**
   - Built-in zero-dependency `.asar` archive unpacker (`crates/tools::rea::asar`).
   - Source-map reconstructor to recover original TypeScript/JavaScript source trees (Deep Mode).
4. **Managed Runtimes (.NET, JVM, Android):**
   - CIL instruction disassembler and metadata inspector for `.NET` assemblies.
   - Headless JADX integration for Android APK decompilation and manifest analysis (Deep Mode).
   - Unity IL2CPP static metadata extractor (`global-metadata.dat` v24–39).
5. **Game Runtimes & Recreations:**
   - Extracts assets, sprite sheets, audio clips, and state logic to recreate gameplay in Godot, Bevy, or WebGL.

---

## 5. The Clean-Room vs. Deep Mode Toggle & Tool Installer

Instead of an array of confusing micro-settings, OpenChat consolidates all reverse engineering and reconstruction capabilities into **one primary toggle**: **Clean-Room Mode vs. Deep Mode**, backed by an automated dependency installer and diagnostic `doctor`.

### 5.1 The Primary Mode Toggle

```text
┌────────────────────────────────────────────────────────────────────────┐
│                        PRIMARY MODE TOGGLE                             │
│                                                                        │
│  [ (•) Clean Room (Default) ]          [ ( ) Deep Mode (Opt-In) ]      │
│                                                                        │
│  • Zero access to private source,       • Full 300+ decompiler &       │
│    decompilers, or internal bytes.        disassembler tool suite.     │
│  • Uses only outward observable        • Ghidra Headless, JADX, ASAR   │
│    behavior: screenshots at 3 screen     unpacking, IL2CPP dumper.     │
│    widths, DOM, public API specs.      • Pseudocode and symbol trees.  │
│  • Refuses decompiler tool calls with   • Requires installed runtime   │
│    an opt-in guidance prompt.            dependencies via `doctor`.    │
└────────────────────────────────────────────────────────────────────────┘
```

- **Clean-Room Mode (Default):** Guarantees complete legal and compliance safety. The agent acts like an external observer—it examines rendered UI, takes screenshots across breakpoints, inspects public REST/GraphQL responses, and reads user-provided specifications. Any tool call that attempts to inspect binary bytes, decompile routines, or extract private code is strictly refused.
- **Deep Mode (Explicit Opt-In):** Activated via the title bar toggle, workspace settings, or `--deep` CLI flag. Unlocks deep binary inspection, Ghidra headless decompilation, JADX APK decompilation, `.asar` unpacking, and low-level AST/symbol extraction for authorized internal reverse engineering and legacy system porting.

### 5.2 Settings UI & Doctor Health Dashboard

```text
┌─────────────────────────────────────────────────────────────────────────────────┐
│  Settings > Engine Mode & Tool Dependencies                                     │
├─────────────────────────────────────────────────────────────────────────────────┤
│                                                                                 │
│  Active Mode:                                                                   │
│  [ (•) Clean Room (Safe Outward Recreation)    ( ) Deep Mode (Full Decompiler) ]│
│                                                                                 │
│  ── Tool Status & Diagnostics (openchat doctor) ─────────────────────────────── │
│                                                                                 │
│  Pure-Rust Engine (300 built-in parsers)                           [ Ready     ]│
│  • Status: Active (ELF, PE, Mach-O, ASAR, HAR, Web)               [ OK ]        │
│                                                                                 │
│  Ghidra Decompiler Engine (NSA Ghidra headless)                    [ Installed ]│
│  • Status: v11.1.2 detected at /opt/ghidra                        [ Recheck ]   │
│  • Java Runtime: OpenJDK 21 detected                              [ Configure ] │
│                                                                                 │
│  JADX (Android APK & Dex Decompiler)                              [ Missing   ]│
│  • Status: Not found in PATH                                      [ Install ⬇ ] │
│  • Target: ~/.openchat/tools/rea/bin/jadx                                       │
│                                                                                 │
│  Tshark / Wireshark (Network Traffic Inspector)                   [ Installed ]│
│  • Status: v4.2.4 detected at /usr/bin/tshark                     [ Recheck ]   │
│                                                                                 │
│  ── Coding Agent Connections ────────────────────────────────────────────────── │
│  Claude Code CLI: [ Connected ]     GitHub Copilot CLI: [ Connected ]           │
│                                                                                 │
│  [ Save Preferences ]                             [ Run Doctor Diagnostics ]    │
└─────────────────────────────────────────────────────────────────────────────────┘
```

### 5.3 Automated Tool Installer Matrix (`crates/tools::rea::installer`)

When Deep Mode is selected, missing tools can be installed with a single click or CLI command (`openchat doctor --fix`):

| Tool | Mode Required | Detection Probe (`doctor`) | Auto-Install Method | Execution Isolation |
|---|---|---|---|---|
| **Pure-Rust Engine** | Clean Room & Deep | Compiled into daemon (`crates/tools`) | Zero-install built-in | Memory-safe Rust |
| **Playwright/Chromium** | Clean Room & Deep | Headless browser probe | Bundled headless binary download | OS sandbox isolation |
| **OpenJDK 21+** | Deep | `java -version` (regex $\ge 21$) | System PM (`brew`, `apt`, `winget`, `choco`) | System environment |
| **Ghidra Headless** | Deep | `ghidraRun` or `$GHIDRA_INSTALL_PATH` | Managed download of official GitHub release zip | Headless CLI sandbox |
| **JADX** | Deep | `jadx --version` | Direct release download into `~/.openchat/tools/rea/bin/` | Read-only volume mount |
| **Tshark / Dumpcap** | Deep | `tshark -v` | System PM (`brew install wireshark`, `apt install tshark`)| Unprivileged PCAP read-only |

### 5.4 Installation & Doctor Event Flow

When a user switches modes or runs `openchat doctor`, diagnostic events stream over WebSocket/SSE:

```json
{
  "type": "rea:doctor_report",
  "active_mode": "clean_room",
  "clean_room_ready": true,
  "deep_mode_ready": false,
  "checks": [
    {"name": "rust_engine", "status": "pass", "version": "0.2.0"},
    {"name": "headless_browser", "status": "pass", "version": "128.0"},
    {"name": "java_runtime", "status": "pass", "version": "21.0.2"},
    {"name": "ghidra", "status": "pass", "version": "11.1.2"},
    {"name": "jadx", "status": "missing", "fix": "Click Install to download JADX release"}
  ]
}
```

---

## 6. Polyglot anti-slop architecture

### 6.1 Goals and non-goals

**Goals**

1. Reduce generic, hedged, placeholder-filled, or incomplete output.
2. Catch implementation gaps in the *proposed diff* before patching files.
3. Keep patches small, attributable, reviewable, and reversible.
4. Work across Python, TypeScript/JavaScript, Go, and Rust first, with a language adapter boundary for future grammars.
5. Keep latency low enough for an interactive coding loop. A sub-5 ms AST inspection budget for small changed files is a **target**, not a universal guarantee; parsing cost scales with changed-file size, grammar, runtime, and host hardware.

**Non-goals**

- Replacing the compiler, type checker, tests, security review, or human review.
- Treating every phrase in a configurable “slop list” as objectively bad.
- Rewriting whole files when a compact contextual patch is sufficient.
- Silently correcting or discarding user code when a check fails.

### 6.2 Layer 1 — Prompt Skill Injector

#### Responsibilities

The injector owns **stack detection, skill selection, and lifecycle policy**. It should not load every rule in a catalog into every request.

1. **Detect stack and active targets.** Fingerprint workspace manifests and file extensions (e.g. `pyproject.toml`, `package.json`, `go.mod`, `Cargo.toml`); inspect the current task and candidate file paths. File/task signals take precedence over a project-wide language list when choosing narrow rules.
2. **Resolve skills.** Match indexed skill metadata to language, framework, file glob, task intent, and project-persisted mode. Fall back to semantic skill search only when deterministic matching is insufficient.
3. **Compose a compact prompt.** Inject only relevant constraints, with an explicit mode such as `during` (guide generation) or `after` (audit/verification). Include token-footprint telemetry and an observable activation event.
4. **Constrain output.** Request a strict patch representation (search/replace blocks or unified diff), exact target paths, and no unchanged-file restatement. Set limits for changed files/lines where appropriate.
5. **After-generation audit.** Use a separate audit instruction to inspect the proposed change for policy omissions, incomplete implementation, scope creep, and test/verification gaps. The audit contributes findings; the deterministic AST gate remains the actual patch gate.

#### Compact patch contract

Prefer a typed patch envelope rather than relying on free-form “find and replace” text:

```json
{
  "format": "search_replace_v1",
  "changes": [
    {
      "path": "src/example.py",
      "old": "def answer():\n    return None\n",
      "new": "def answer():\n    return compute_answer()\n"
    }
  ]
}
```

Rules for safe application:

- Require exact old-text match and exactly one match by default; ambiguity is a rejection, not a guess.
- Bind each operation to the file hash/revision seen by the model. Reject stale patches and ask for regeneration/rebase.
- Permit an explicit unified-diff mode for multi-hunk edits; parse and validate it before touching disk.
- Apply all files atomically where possible, or stage to temporary files and roll back the set on any error.
- Keep the diff as the source of truth for AST checks and Dev Mode provenance.

### 6.3 Layer 2 — Logit Backtracking Sampler

#### Why backtracking rather than token-wide bias

A phrase may span multiple tokenizer pieces. Penalizing the first token of a phrase can suppress many valid continuations. The sampler instead waits until a banned phrase/pattern is recognized in the generated text, identifies the earliest token boundary that enabled it, restores a cached prefix, adjusts the probabilities of the offending continuation, and samples again.

#### Proposed operation

1. Keep a bounded generation history of token IDs, decoded text spans, and the probability/logit state needed to resume.
2. Match configured phrases with a trie/Aho–Corasick-style matcher, or use bounded regex matching where the runtime supports safe rewind.
3. On a match, map character offsets back to token spans.
4. Rewind to the match start (or the earliest relevant cached token boundary).
5. Down-weight or ban the specific continuation, not every token sharing a prefix.
6. Resample, with a maximum retry/rewind budget to avoid loops.
7. Record a structured event for observability, avoiding storage of private prompt content unless explicitly enabled.

#### Runtime compatibility

- **Local runtimes:** integrate where raw logits/probabilities, token-by-token continuation, and sampler state are exposed (e.g. a compatible Transformers-based serving loop or an inference engine with equivalent hooks).
- **Compatible server/runtime:** an OpenAI-compatible façade can sit in front of an instrumented local model, but the façade alone does not create access to logits.
- **Cloud API without logits/hooks:** do not claim true sampler enforcement. Use prompt constraints, output screening, and patch/AST gates; expose sampler status as `unavailable` rather than implying it ran.

### 6.4 Layer 3 — Polyglot Tree-Sitter AST Gate

The AST gate is a **pre-patch quality barrier** over changed syntax. It parses only changed files/hunks in memory, while retaining enough surrounding context to identify enclosing statements/functions.

#### Candidate checks

| Language | Examples of suspicious constructs | Notes |
|---|---|---|
| Python | `pass` in a newly added function/branch, `...` ellipsis placeholder, `NotImplementedError`, obvious dummy literal returns | `pass` may be valid in marker classes, exceptions, or intentional no-op hooks; use context/profile rules. |
| TypeScript / JavaScript | empty `catch {}`, empty function bodies, `return null` / fixed mock values in production paths, `throw new Error("TODO")` | Empty callbacks and intentional adapters are possible; severity depends on context. |
| Go | empty function bodies, `panic("TODO")`, hard-coded mock return values, TODO/FIXME markers | Empty methods may satisfy interfaces intentionally; inspect declaration and comments. |
| Rust | `todo!()`, `unimplemented!()`, `panic!("TODO")`, placeholder `Default::default()` / constant dummy data | `todo!()` and `unimplemented!()` are strong blockers in production patch paths; tests/examples may be allowed by policy. |

#### Gate policy

- **Block:** explicit unresolved stubs in changed production code (`todo!()`, `unimplemented!()`, clear generated placeholder exceptions, disallowed empty implementation bodies).
- **Warn / require justification:** dummy dictionaries/objects, fixed mock returns, suspicious `pass`, empty catches, generic fallback branches.
- **Inform:** TODO/hedging/deferment in comments or tests, depending on profile.
- **Allow by configuration:** test fixtures, generated files, migrations, examples, explicit allowlist/inline suppression with reason.

---

## 7. Multi-client sync protocol & state invariants (OpenChamber-grounded)

OpenChamber\'s frontend/backend architecture pioneered a sophisticated session synchronization system across directories and spaces (`packages/ui/src/sync` and `.agents/skills/sync-state-invariants`). In OpenChat\'s headless Rust daemon rewrite, we adopt and formalize these state invariants to ensure seamless synchronization across CLI, Web, Telegram, and Discord clients.

### 7.1 Sources of truth and authority

| Store / Input | Authority | Scope | Invariant & Valid Use |
|---|---|---|---|
| **Directory Child Store** (`crates/session`) | Authoritative live state | Per-workspace directory | Active turns, running processes, streaming tokens, AST verification gates. |
| **Global Session Index** (`crates/session`) | Authoritative directory cache | Cross-directory / Daemon-wide | Sidebar summaries, session admissions, project switcher, archive states. |
| **Persisted SQLite Store** (`crates/memory`) | Authoritative history | Local durable disk (WAL) | Continuity across daemon restarts; never treated as proof of current live execution. |
| **Optimistic Shadow Tracker** (`crates/core`) | Non-authoritative shadow | In-flight turns / pending diffs | Render-continuity and sub-millisecond client echo; removed on error or overwritten on authoritative commit. |

### 7.2 Core sync state invariants

1. **"Failure is not empty":** Any loader whose result can replace, delete, or clear state must distinguish a failure from successful empty data. Never swallow an API or DB error into an empty list (`[]`). A failed directory reload preserves prior state and marks that directory degraded; it never silently clears sessions.
2. **Authoritative disappearance vs deletion:** A session or file disappearance between snapshots is only treated as a deletion after an established authoritative baseline exists. Never infer deletion from the first snapshot, an empty startup state, or a partially filtered query.
3. **Monotonic revision ordering:** All persistent mutations attach a monotonic revision number. In-flight responses or delayed background fetches cannot overwrite mutations newer than the request revision.
4. **Optimistic shadow pairing:** Every client-initiated turn or patch generates an optimistic mutation echoed locally. The daemon acknowledges mutations with client IDs; on failure, both visible and shadow entries are purged atomically.
5. **Never evict what is in use:** An active session, AST parse cache, or file snapshot acquired during execution cannot be evicted by memory-governor limits until the active turn commits or cancels. Capacity limits are soft targets handled during idle scans.
6. **Fan-out avoidance:** Refresh only directories that are actively selected or explicitly named in a change event. Polling or broadcasting across unselected workspaces causes an unnecessary fan-out of background watchers and compiler processes.

---

## 8. Unified orchestration lifecycle

### 8.1 End-to-end flow

```text
 User request + target (URL, app, binary, or repo)
               │
               ▼
 ┌──────────────────────────────┐
 │ Mode Check / doctor probe    │  determine Clean-Room vs Deep; verify tools
 └───────────────┬──────────────┘
                 ▼
 ┌──────────────────────────────┐
 │ Study Phase                  │  Clean-Room: screenshots, DOM, public contracts
 │ (crates/tools::rea)          │  Deep: disassembler, symbols, ASAR, decompilation
 └───────────────┬──────────────┘
                 ▼
 ┌──────────────────────────────┐
 │ Build Phase (Coding Agent)   │  Claude Code / Copilot / Grok / OpenChat via ACP/MCP,
 │                              │  clean-room prompt rules, compact search/replace diff
 └───────────────┬──────────────┘
                 ▼
 ┌──────────────────────────────┐
 │ Pre-commit AST Gate          │  Tree-sitter checks changed syntax in-memory (no todo!(),
 │ (crates/core)                │  no empty stubs); repairs if rejected
 └───────────────┬──────────────┘
                 ▼
 ┌──────────────────────────────┐
 │ Check Phase (Verification)   │  compare responsive visual diffs (3 screen widths),
 │                              │  run test vectors, calculate similarity score
 └───────────────┬──────────────┘
                 ▼
 ┌──────────────────────────────┐
 │ Ready Phase                  │  launch preview / play-test, ask for iterative changes
 └──────────────────────────────┘
```

### 8.2 Rust backend crate mapping (`cmspeedau/open-chat`)

| Lifecycle Phase | Responsible Crate | Key Functionality |
|---|---|---|
| **Bootstrap, Sync & Triage** | `crates/session` | Manages `setup_beat`, directory child stores, monotonic sync revisions, and small-model diagnostic offloading. |
| **Skill Index & Vector Store** | `crates/memory` | Stores synthesized `SKILL.md` contracts and project conventions using `rusqlite` + `sqlite-vec`. |
| **Mode & Tool Engine** | `crates/tools` | Implements the 300+ pure-Rust tools, Ghidra/JADX bridges, ASAR unpacker, and `doctor` installer manager (`crates/tools::rea::installer`). |
| **AST Inspection & Patching** | `crates/core` | Parses search/replace diffs in-memory, executes Tree-sitter queries, manages atomic commits and shadow updates. |
| **Inference Control** | `crates/governor` | Manages local backtracking sampler, concurrency semaphores, process priority. |
| **Workspace File Watcher** | `crates/watcher` | Employs debounced `notify` to track file modifications and invalidate AST caches. |

---

## 9. Interfaces and implementation sketches

### 9.1 Rust AST gate sketch (`crates/core`)

```rust
use std::path::Path;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum GateError {
    #[error("Parse error for {0}")]
    Parse(String),
    #[error("File stale or not found: {0}")]
    NotFound(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Severity {
    Block,
    Warning,
    Info,
}

#[derive(Debug, Clone)]
pub struct Finding {
    pub rule_id: String,
    pub severity: Severity,
    pub path: String,
    pub line: usize,
    pub message: String,
}

pub trait AstGate: Send + Sync {
    fn inspect(&self, path: &Path, content: &str) -> Result<Vec<Finding>, GateError>;
}
```

### 9.2 Mode-Aware Craft Project Schema (`craft.json`)

```rust
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ProjectMode {
    CleanRoom,
    Deep,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum TargetKind {
    Website { url: String },
    NativeBinary { path: PathBuf, arch: String, format: String },
    ElectronAsar { path: PathBuf, app_name: String },
    GameExecutable { path: PathBuf, engine: Option<String> },
    AndroidApk { path: PathBuf, package_id: String },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CraftProject {
    pub id: String,
    pub request: String,
    pub mode: ProjectMode,
    pub target: TargetKind,
    pub target_folder: PathBuf,
    pub status: StepStatus,
    pub similarity_score: Option<f32>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum StepStatus {
    Study { progress: f32 },
    Build { agent: String },
    Check { checks_passed: usize, checks_total: usize },
    Ready { preview_url: Option<String> },
}
```

---

## 10. OpenChat Dev Mode & small-model diagnostics offload

### 10.1 Product concept & main-model offload

**Dev Mode** is an opt-in, self-monitoring diagnostic assistant within OpenChat. When an operational failure occurs—such as a stream crash, recurring tool failure, or repeated AST gate rejection—**the main LLM does not consume its own context and cloud token budget to troubleshoot itself.**

Instead, OpenChat **offloads diagnostics to the small local model (Tier 0)**:

```text
       ┌─────────────────────────────────────────────────────────┐
       │                Tier 1: Main LLM Loop                    │
       │  (Focuses 100% on user coding task; unaffected by logs) │
       └────────────────────────────┬────────────────────────────┘
                                    │ Incident Occurs
                                    ▼
       ┌─────────────────────────────────────────────────────────┐
       │     crates/session: Dev Mode Diagnostic Ring Buffer     │
       │  (Captures error code, sanitized trace, tool args)      │
       └────────────────────────────┬────────────────────────────┘
                                    │ Offload triage (Async / 0 cloud tokens)
                                    ▼
       ┌─────────────────────────────────────────────────────────┐
       │        Tier 0: Small Local Model (Qwen/Llama 1B-3B)     │
       │                                                         │
       │  1. Clusters recurring error signatures                 │
       │  2. Strips paths, secrets, & token dumps                │
       │  3. Analyzes failure hypothesis & reproduction steps   │
       │  4. Compiles structured GitHub Issue Draft              │
       └────────────────────────────┬────────────────────────────┘
                                    │
                                    ▼
                       User Confirmation Card in UI
                    [Review] [Edit] [Create GitHub Issue]
```

#### Why offloading to the small model is essential:
1. **Zero context pollution:** Long stack traces and error dumps are notoriously noisy. Injecting a 200-line crash trace into the main coding conversation burns valuable context tokens and distracts the primary model from the user\'s ongoing task.
2. **Zero token cost:** Incident analysis and log clustering do not require high-end frontier reasoning; a fast 1.5B or 3B local model handles trace normalization, clustering, and issue drafting entirely offline at zero cost.
3. **Survives cloud API disconnects:** If the failure is caused by an expired API key, network timeout, or provider 500 error, the main cloud model cannot diagnose the issue because it is unreachable. The local small model stays operational and diagnoses the outage locally.

### 10.2 User-confirmation card and workflow

Example Web UI card:

```text
┌────────────────────────────────────────────────────────────────┐
│ Dev Mode found a recurring tool failure                        │
│ Signature: stream.parse.unexpected_eof  •  4 occurrences        │
│ Triaged by: Local bootstrap model (0 cloud tokens used)        │
│ Likely area: streaming response parser (hypothesis)              │
│                                                                │
│ Proposed issue: “Streaming parser fails on truncated event”     │
│ Target: owner/repository (choose or confirm)                     │
│ Included: error class, timestamps, redacted 12-line trace        │
│ Excluded: prompts, source content, tokens, environment variables  │
│                                                                │
│ [Review draft] [Edit] [Create GitHub issue] [Dismiss]            │
└────────────────────────────────────────────────────────────────┘
```

Confirmation requirements:

- Opening the card, generating a draft, and publishing are separate steps.
- “Create” is a deliberate, explicit action; no pre-checked confirmation and no background auto-submit.
- Show target repository, title/body, labels, assignees, and every attachment/export field before dispatch.
- Allow redaction, editing, copying without publishing, and discard.

---

## 11. OpenChat / OpenChamber rewrite direction

### 11.1 Core product shape

OpenChat is conceived as a next-generation agentic workspace and rewrite inspired by OpenChamber, emphasizing a **headless, fast core daemon** rather than a UI-bound agent process. A single execution core can serve lightweight clients:

- Web workspace for interactive coding and inspection.
- Telegram and Discord bridges for notifications, approvals, and remote sessions.
- CLI/API (`openchat-cli`) for headless workflows, MCP serving, and automation.
- A persistent daemon that continues work when a browser tab closes.

### 11.2 Native scoped memory

A local embedded memory system addresses cross-session amnesia without stuffing every past conversation into the prompt:

- **Global user memory** (e.g. `~/.openchat/memory.sqlite`): stable preferences and defaults.
- **Project memory** (e.g. `.openchat/memory.sqlite`): stack, test commands, architectural decisions, conventions, and project skill bindings.
- **Session working memory** (RAM/WAL or bounded persisted scratch state): task checkpoints, short-lived traces, and active plan state.
- **Sparse retrieval:** SQLite FTS5 for exact names, commands, symbols, and keywords.
- **Dense retrieval:** an embedded vector extension such as `sqlite-vec` or a local alternative for semantic recall.
- **Hybrid ranking:** merge sparse and dense results (e.g. reciprocal rank fusion), then select a small, explicit token budget per turn.

---

## 12. Roadmap and integration sequence

### Phase 0 — Contracts, measurement & bootstrap concierge
- Define patch envelope, event schemas, diagnostic privacy policy, workspace-root/path policy, and per-project configuration.
- Implement the Tier-0 lightweight local bootstrap model and `setup_beat` flows for zero-cloud onboarding.

### Phase 1 — Safe patch pipeline
- Implement exact-match and unified-diff parsing, base-hash validation, atomic staging/rollback, and rejection UX in `crates/core`.
- Add Tree-sitter adapters for Python, TypeScript/JavaScript, Go, and Rust.

### Phase 2 — Skill injection & AST-grounded synthesis
- Add workspace stack fingerprinting, skill metadata index, `during` / `after` composition, and visible activation events.
- Implement repository-grounded skill synthesis reading manifests, AST conventions, and local compiler symbols.

### Phase 3 — Clean-Room vs. Deep Mode & Craft-Anything integration
- Implement `crates/tools::rea` with the 4-stage engine: **Study → Build → Check → Ready**.
- Build the **Clean-Room vs. Deep Mode** toggle switch and CLI flags (`--clean-room` / `--deep`).
- Implement the `openchat doctor` diagnostic command and automated tool installer manager (`crates/tools::rea::installer`) for Ghidra, JADX, Chromium, and Tshark.
- Introduce responsive visual diff checks (3 screen widths) and test vector verification.

### Phase 4 — Sampler integration
- Implement the backtracking sampler in `crates/governor` for local inference runtimes exposing logits and rewindable state.

### Phase 5 — Daemon, memory, sync invariants & Dev Mode foundation
- Build daemon session lifecycle (`crates/session`), multi-client sync protocol, directory child stores, monotonic revisions, and memory scopes (`crates/memory`).
- Offload incident clustering, stack-trace normalization, and failure root-cause analysis to Tier-0 local model.

### Phase 6 — Confirmed GitHub dispatch and multi-client workflows
- Implement review/edit card and explicit approval object bound to draft revision/repository.
- Add API/MCP dispatcher with least-privilege access, idempotency, and a copy-only fallback.

### Phase 7 — Scale and polish
- Add further language adapters, multi-agent resource governor, shared watcher (`crates/watcher`), and virtualized long-session UI.

---

## 13. Safety, failure modes, and design decisions

### 13.1 Enforcement boundaries

| Capability | What it can enforce | Important limitation |
|---|---|---|
| Bootstrap concierge & Diagnostic coprocessor | Zero-cost setup, hardware sizing, manifest detection, trace clustering & draft issue generation | Limited reasoning capability compared to frontier models; restricted to onboarding beats and structured diagnostic summarization. |
| Clean-Room Mode | Strict prohibition against reading private code or decompiling binaries; strictly uses visible screenshots & public contracts | Cannot reverse internal logic not visible in public UI or wire traffic. |
| Deep Mode | Ground-truth behavioral evidence from binaries, ASAR, APK, and network traces | Reverse-engineering outputs require clean-room validation; external tools subject to host system package manager limits. |
| Prompt injector | Output format, preferred practices, selected project rules | Model may ignore or misunderstand instructions. |
| Backtracking sampler | Selected phrase/token continuations in a hook-enabled generator | Requires logits and rewind state; many hosted APIs do not expose these. |
| AST gate | Syntax-aware detection of configured constructs in changed content | Does not prove semantic correctness, safe behavior, or successful compilation. |

---

## 14. References and provenance

The architecture is grounded in and inspired by these reference projects:

- [`storytold/craft-anything`](https://github.com/storytold/craft-anything) — The foundational pure-Rust engine for reverse-engineering websites, apps, and games. Establishes the 4-step **Study → Build → Check → Ready** lifecycle, the strict **Clean-Room vs. Deep Mode** dichotomy, responsive checks at 3 screen widths, and the CLI/desktop `doctor` tool installer architecture.
- [`morluto/rea`](https://github.com/morluto/rea) — Reverse Engineer Anything: unified agent MCP/tools for native binaries, JavaScript/Electron ASAR, mobile APKs, network traces, and clean-room reconstruction.
- [`openchamber/openchamber`](https://github.com/openchamber/openchamber) — multi-client sync protocol, directory child stores, session reconciliation invariants (`sync-state-invariants`), and inspiration for the headless rewrite.
- [`NousResearch/hermes-agent`](https://github.com/NousResearch/hermes-agent) — two-tier architecture, small model onboarding flow, and `setup_beat` interactive card orchestration.
- [`miqdadbadjuber/anti-slop`](https://github.com/miqdadbadjuber/anti-slop) — lifecycle-aware prompt/skill constraints and anti-slop concepts.
- [`sam-paech/antislop-sampler`](https://github.com/sam-paech/antislop-sampler) — phrase matching, probability adjustment, and backtracking at generation time.
- [`skew202/antislop`](https://github.com/skew202/antislop) — Tree-sitter and text-pattern-based detection of placeholders, deferrals, hedging, stubs, and dummy data across languages.
