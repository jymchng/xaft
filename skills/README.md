# xaft Skills Directory

## Purpose

This directory is the **AI-native repository intelligence layer** for the xaft Rust coding agent runtime. Unlike traditional documentation that targets human readers with prose and diagrams, these "skills" files are structured to be consumed by AI agents—particularly coding assistants, code reviewers, and automated refactoring tools—that need deep, precise, machine-parseable knowledge about how xaft works internally.

Each skill document encodes not just *what* the system does, but *why* it does it that way, *what invariants must hold*, and *how to extend it without breaking things*. This makes the skills directory a living specification that any AI agent can read to immediately orient itself within the codebase, understand architectural decisions, and produce correct modifications. The goal is to eliminate the "cold start" problem where an AI agent unfamiliar with xaft would otherwise need to read thousands of lines of code before making a safe change.

Think of this directory as the compressed institutional knowledge of the xaft project. When a new contributor—or a new AI context window—opens this repository, reading the skills files should bring them to full operational understanding faster than any combination of code reading and API documentation browsing. Every section is written with the constraint that a consumer must be able to act on the information without further clarification.

---

## How to Use This Directory

1. **Start with the mental model.** Read `architecture/01-mental-model.md` first. It lays out the big-picture flow: how the runtime boots, how the orchestrator drives agents through handoffs, how signals flow to the TUI, and how sessions persist. Everything else builds on this foundation.

2. **Then read crate responsibilities.** `architecture/02-crate-responsibilities.md` tells you which crate owns what. Before modifying any code, consult this document to confirm you are editing the right crate. Cross-crate changes are rare and require coordination; this file makes those boundaries explicit.

3. **Consult extension points before adding features.** `architecture/03-extension-points.md` catalogs every place where the system is designed to be extended. If you want to add a tool, an agent, a provider, a signal, or a TUI widget, this document tells you exactly which trait to implement and which registry to register with.

4. **Trace execution paths through the runtime docs.** `runtime/01-entry-points.md` and `runtime/02-orchestration-flow.md` map the journey from `main()` to agent completion. Use these when debugging flow issues, adding observability, or understanding why a particular agent receives certain tools.

5. **Understand agents deeply before modifying agent behavior.** `agents/01-lifecycle.md` covers every hook in an agent's existence, and `agents/02-composition.md` shows how to compose new agents using the builder APIs. Together, they give you complete control over agent behavior without needing to modify the core agent loop.

---

## Navigation Guide

```
skills/
├── README.md                          ← You are here
├── architecture/
│   ├── 01-mental-model.md             ← Big-picture runtime flow and data flow
│   ├── 02-crate-responsibilities.md   ← Which crate owns what, dependency edges
│   └── 03-extension-points.md         ← Every trait-based extension point cataloged
├── runtime/
│   ├── 01-entry-points.md             ← All ways to enter the runtime, tracing from main()
│   └── 02-orchestration-flow.md       ← Orchestrator internals, handoff mechanics, tool assignment
└── agents/
    ├── 01-lifecycle.md                ← Full agent lifecycle from construction to completion
    └── 02-composition.md              ← AgentBuilder and PlanAgentBuilder fluent APIs
```

### Reading Order by Role

| Role | Recommended Path |
|------|-----------------|
| **New contributor** | README → 01-mental-model → 02-crate-responsibilities → all others |
| **Feature author (new tool)** | 03-extension-points → 01-lifecycle → 02-orchestration-flow |
| **Feature author (new agent)** | 03-extension-points → 02-composition → 02-orchestration-flow |
| **Bug fixer (flow issue)** | 01-entry-points → 02-orchestration-flow → 01-lifecycle |
| **Bug fixer (TUI issue)** | 03-extension-points (TUI section) → 01-mental-model (SignalBus) |
| **Refactorer** | 02-crate-responsibilities → 03-extension-points → all runtime + agents |

---

## Document Structure Convention

Every skill document in this directory follows the same structure to ensure consistency and completeness:

- **Purpose** — Why this document exists and what question it answers.
- **Mental Model** — The conceptual framework you need before reading details.
- **Architecture Explanation** — How the relevant subsystem is structured and why.
- **Extension Patterns** — How to extend the system at the points covered by this document.
- **Common Pitfalls** — Mistakes others have made, with explanations of why they fail.
- **Invariants** — Properties that must always hold; violating them is undefined behavior.
- **Lifecycle Expectations** — What happens over time: initialization, steady state, shutdown.
- **Examples** — Concrete code snippets and configuration showing real usage.
- **Implementation Guidance** — Step-by-step instructions for common tasks.

Each section contains at least 150 words of substantive content. If a section feels thin, that is a bug in the documentation—please file an issue or submit a PR.

---

## Contributing to Skills

When adding a new skill document, follow the structure convention above. Write for an AI consumer first and a human reader second—that means preferring precise terminology, explicit invariants, and code references over vague prose. Every claim should be verifiable against the source code. If the code changes and a skill document becomes inaccurate, the document is more important to update than a comment in the code, because more consumers depend on it.
