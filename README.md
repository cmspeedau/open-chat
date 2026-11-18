# open-chat

Next-generation agentic workspace & rewrite inspired by [OpenChamber](https://github.com/openchamber/openchamber).

`open-chat` transitions from an Electron/mobile-heavy model to a **headless, ultra-fast core daemon** supporting first-class multi-interface clients (Telegram, Discord, Web) with **native persistent memory** and **observable, generalized skill lifecycle bindings**.

---

## 🔗 Reference Projects & Ecosystem

- **Base Reference:** [openchamber/openchamber](https://github.com/openchamber/openchamber) — Headless core architecture decoupling the engine from mobile/Electron wrappers.
- **Skill Reference:** [miqdadbadjuber/anti-slop](https://github.com/miqdadbadjuber/anti-slop) — Exemplar ruleset illustrating stage-based operational modes (`during` vs `after`) across design, copy, and code generation.

---

## 🧠 Architectural Pillar 1: Inbuilt Native Memory

A key limitation in standard agent wrappers is memory amnesia across sessions and unprincipled prompt bloat. `open-chat` introduces an embedded, dual-tier local memory architecture:

### 1. Dual-Tier Storage Engine
- **Structured & Keyword Search (FTS5):** SQLite FTS5 for exact keyword matching, project metadata, tool configurations, and architectural rules.
- **Dense Vector Search (`sqlite-vec` / LanceDB):** Embedded vector storage without external Docker requirements for semantic retrieval of project conventions, patterns, and past debugging logs.
- **Hybrid Retrieval (RRF / Cross-Encoder):** Reciprocal Rank Fusion merging dense and sparse results with a 500–1,000 token budget injected per turn.

### 2. Scoped Memory Tiers
1. **Global User Memory (`~/.openchat/memory.sqlite`):**
   - Personal preferences (formatting, tone, keybindings, style guidelines).
2. **Project Workspace Memory (`.openchat/memory.sqlite`):**
   - Tech stack details, architectural constraints, test commands, active project skills & triggers.
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

## ⚡ Architectural Pillar 2: Universal Skill Lifecycle & Intent Binding Engine

Rather than hardcoding rules for one specific repository, `open-chat` provides a **universal skill specification schema**. Any skill or skill pack using declarative phase triggers (e.g., `during`, `after`, `review`, `audit`, `plan`) is automatically understood, persisted, and loaded on-demand.

### 1. Universal Skill Schema (`SKILL.md` frontmatter)
Any skill package conforming to the open schema defines its lifecycle triggers and sub-skill bindings:

```yaml
---
name: anti-slop # or tdd-workflow, security-guard, clean-architecture, etc.
description: Eliminates generic patterns and enforces strict craftsmanship standards.
modes:
  during:
    description: Real-time generation guardrails applied while producing code, copy, or UI.
    autoload: true
    subskills:
      - id: antislop-ui
        triggers: [".tsx", ".jsx", ".vue", ".svelte", ".html", ".css", "component", "styling"]
      - id: antislop-code
        triggers: [".rs", ".ts", ".go", ".py", "refactor", "function", "logic"]
      - id: antislop-copywriting
        triggers: [".md", "copy", "marketing", "content", "documentation"]
  after:
    description: Post-generation audit and remediation pass.
    autoload: false
    entrypoint: antislop-audit
---
```

### 2. Conversational Intent Binding & Persistence
When a user says:
> *"during"*  
> *"use anti-slop during this project"*  
> *"run tdd-workflow during"*  

1. **Intent Parser & Phase Resolver:**
   - The daemon detects lifecycle keyword cues (`during`, `after`, `plan`, `audit`).
   - If an active skill pack is referenced (or default workspace rule applies), it binds that mode.
2. **Project Workspace Storage (`.openchat/memory.sqlite`):**
   The binding is committed to the local workspace database:
   ```json
   {
     "project_id": "my-app",
     "active_skill_bindings": {
       "anti-slop": {
         "mode": "during",
         "bound_at": "2026-11-19T09:40:00Z",
         "autoload": true
       }
     }
   }
   ```
3. **Zero-Prompt Session Restoration:**
   - Across restarts, new chat sessions, or context clears in this workspace, the daemon reads the binding.
   - The user never has to re-prompt *"remember to use X"* or append manual skill flags.
4. **Contextual Dispatch on Demand:**
   - The daemon monitors active context (e.g., file paths being modified, user prompt intent).
   - If editing `Navbar.tsx`, it dynamically binds the `antislop-ui` sub-skill.
   - If modifying backend endpoints, it activates `antislop-code`.

### 3. Observable Lifecycle Events & Visual Badges
Skills are never silently injected. The engine emits an explicit WebSocket/SSE event:

```json
{
  "type": "agent:skill_activated",
  "skill": "anti-slop",
  "mode": "during",
  "active_subskills": ["antislop-ui"],
  "source": ".openchat/skills/anti-slop/SKILL.md",
  "trigger": "User prompt modifying frontend component (Navbar.tsx)"
}
```

- **Web UI:** Live status pill: `🛡️ anti-slop [during] • UI active`
- **Telegram & Discord:** Visual indicator preamble: `⚡ [Active Mode: anti-slop (during)]`

### 4. Dynamic Agent Tool: `load_skill`
Agents can dynamically inspect the workspace skill catalog and invoke:
```json
load_skill({ "skill": "anti-slop", "subskill": "antislop-code", "mode": "during" })
```
This leaves an explicit tool execution trace in the chat stream so the user always sees what rules are loaded.

---

## 🌐 Architectural Pillar 3: Multi-Client Core Daemon

Instead of managing native mobile applications, `open-chat` provides a unified headless daemon exposing:
- **Telegram Bot Bridge:** Complete agent execution and monitoring on mobile devices via native Telegram chat with inline action buttons.
- **Discord Bot Bridge:** Multi-channel or thread-based session management for collaborative or personal workflows.
- **Lightweight Web UI:** Clean, responsive workspace client for desktop browsers.

---

## 🗺️ Roadmap & Next Steps

- [ ] Core daemon scaffolding (Rust / TypeScript).
- [ ] SQLite + FTS5 + `sqlite-vec` memory persistence layer.
- [ ] Universal Skill Lifecycle & Mode Engine (`during` / `after` / `plan`).
- [ ] Auto-binding listener based on file extensions and intent triggers.
- [ ] Event-driven skill loader & `load_skill` tool definition.
- [ ] Telegram bot adapter integration.
- [ ] Web UI frontend client.
