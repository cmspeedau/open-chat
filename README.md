# open-chat

Next-generation agentic workspace & rewrite inspired by [OpenChamber](https://github.com/openchamber/openchamber).

`open-chat` transitions from an Electron/mobile-heavy model to a **headless, ultra-fast core daemon** supporting first-class multi-interface clients (Telegram, Discord, Web) with **native persistent memory** and **observable, context-aware skill execution**.

---

## 🔗 Reference Projects & Ecosystem

- **Base Reference:** [openchamber/openchamber](https://github.com/openchamber/openchamber) — Headless core architecture decoupling the engine from mobile/Electron wrappers.
- **Skill Reference:** [miqdadbadjuber/anti-slop](https://github.com/miqdadbadjuber/anti-slop) — Specialized ruleset eliminating generic AI-generated UI designs, copy, and code patterns across development lifecycle stages.

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

## ⚡ Architectural Pillar 2: Observable Skills System & Workflow Trigger Modes

In traditional setups, skill injection is often silent and opaque—skills are stuffed into system prompts without confirmation, leaving the user guessing whether rules were loaded.

`open-chat` makes skills **explicit, observable, inspectable, and persistent per project**:

### 1. Project Memory & Workflow Mode Persistence (e.g., "During" vs "After")
Skills like `anti-slop` define specific operating modes:
- **`During` Mode:** Real-time generation guardrails (prevents AI-slop UI components, filler copy, and boilerplate comments while writing code).
- **`After` Mode:** Audit and remediation pass over existing codebases.

When the user says:
> *"During"* or *"Use anti-slop during this project"*

1. **State Persistence:** The daemon stores the mode directly into Project Workspace Memory (`.openchat/memory.sqlite`):
   ```json
   {
     "project": "my-app",
     "active_skills": {
       "antislop": {
         "mode": "during",
         "autoload": true,
         "subskills": ["antislop-ui", "antislop-code", "antislop-copywriting"]
       }
     }
   }
   ```
2. **Silent Zero-Prompt Autoloading:** For subsequent prompts in this workspace, the agent loads the relevant skills automatically without the user needing to repeat instructions or append `/skill-name` tokens.
3. **Task-Specific Dynamic Loading:**
   - Writing UI components (`.tsx`, `.vue`, `.html`) $\rightarrow$ auto-binds `antislop-ui` and `antislop-layoutmobile`.
   - Writing docs or copy $\rightarrow$ auto-binds `antislop-copywriting`.
   - Editing logic/code $\rightarrow$ auto-binds `antislop-code` comment hygiene.

### 2. Skill Activation Events & Visible Feedback
- The daemon emits an explicit `agent:skill_activated` lifecycle event across WebSocket/SSE whenever a skill binds to context:
  ```json
  {
    "type": "agent:skill_activated",
    "skill": "antislop",
    "mode": "during",
    "subskills": ["antislop-ui", "antislop-code"],
    "source": ".agents/skills/antislop/SKILL.md",
    "reason": "Project memory: active mode is 'during'"
  }
  ```
- **Visual Feedback:** 
  - **Web UI:** Collapsible status badges showing active skills and their token footprints (`🛡️ antislop [during] active: antislop-ui, antislop-code`).
  - **Telegram & Discord:** Small preambles/tags (e.g., `⚡ [Active: antislop (during)]`).

### 3. On-Demand Skill Tool Calls (`load_skill`)
- Agents can also explicitly invoke `load_skill(skill_name)` dynamically, producing a visible tool call event in chat history.

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
- [ ] Project-level skill trigger store (handling persistent modes like `during` and `after`).
- [ ] Event-driven skill loader & `load_skill` tool definition.
- [ ] Telegram bot adapter integration.
- [ ] Web UI frontend client.
