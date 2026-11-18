# open-chat

Next-generation agentic workspace & rewrite inspired by [OpenChamber](https://github.com/openchamber/openchamber).

`open-chat` transitions from an Electron/mobile-heavy model to a **headless, ultra-fast core daemon** supporting first-class multi-interface clients (Telegram, Discord, Web) with **native persistent memory** and **observable skill execution**.

---

## 🔗 Reference Project

- **Base Reference:** [openchamber/openchamber](https://github.com/openchamber/openchamber)
- **Design Philosophy:** Decouple the core execution daemon from the client presentation layer. Replace mobile app wrappers (Android/iOS) with chat platform bots (Telegram, Discord) and lightweight web interfaces.

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
   - Tech stack details, architectural constraints, test commands, project quirks.
3. **Session Working Memory (RAM/WAL):**
   - Active scratchpad, task checkpoints, intermediate debugging traces.

### 3. Asynchronous Lifecycle & Memory Consolidation
- Background reflection pass executed at end-of-turn or on idle.
- Automatic contradiction resolution (e.g., state management refactors overwrite previous conventions rather than duplicating).
- Native agent tools for self-management:
  - `memory_store(category, key, value)`
  - `memory_recall(query)`
  - `memory_forget(key)`

---

## ⚡ Architectural Pillar 2: Observable Skills System

In traditional setups, skill injection is often silent and opaque—skills are stuffed into system prompts without confirmation, leaving the user guessing whether rules were loaded.

`open-chat` makes skills **explicit, observable, and inspectable**:

### 1. Skill Activation Events
- The daemon emits an explicit `agent:skill_activated` lifecycle event across WebSocket/SSE:
  ```json
  {
    "type": "agent:skill_activated",
    "skill": "docker-deployment",
    "source": ".agents/skills/docker/SKILL.md",
    "reason": "Triggered by keyword 'containerize'"
  }
  ```
- **Visual Feedback:** 
  - **Web UI:** Collapsible status badges showing active skills and their token footprints.
  - **Telegram & Discord:** Small preambles/tags (e.g., `⚡ [Loaded Skill: Docker Deployment]`).

### 2. On-Demand Skill Tool Calls (`load_skill`)
- Skills are not unconditionally dumped into the prompt upfront.
- Agents explicitly invoke `load_skill(skill_name)` dynamically, producing a visible tool call event.

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
- [ ] Event-driven skill loader & `load_skill` tool definition.
- [ ] Telegram bot adapter integration.
- [ ] Web UI frontend client.
