# Table of Contents: Phase Memory Bank

**Last Updated**: 2025-11-26
**Version**: 0.2

---

## Overview

This Memory Bank contains all project knowledge, decisions, patterns, and progress tracking for Phase development (MVP + Phase Boot).

**Navigation**: This TOC is your starting point. Jump to specific files based on what you need.

---

## Core Files (Read First)

| File | Purpose | Update Frequency | Last Updated |
|------|---------|------------------|--------------|
| [projectbrief.md](./projectbrief.md) | Vision, goals, MVP scope | Rarely (major pivots) | 2025-11-08 |
| [systemPatterns.md](./systemPatterns.md) | Architecture patterns | Pattern discovery | 2025-11-08 |
| [techContext.md](./techContext.md) | Tech stack decisions | New tech adoption | 2025-11-08 |
| [activeContext.md](./activeContext.md) | Current sprint focus | Weekly | 2025-11-08 |
| [progress.md](./progress.md) | Status, blockers | Weekly | 2025-11-08 |
| [projectRules.md](./projectRules.md) | Coding standards | New patterns | 2025-11-08 |
| [decisions.md](./decisions.md) | Architectural decisions (ADRs) | Arch decisions | 2025-11-08 |
| [quick-start.md](./quick-start.md) | Common patterns, session data | Weekly | 2025-11-08 |

---

## File Guide: When to Read Each File

### Starting a New Session
1. **quick-start.md** - Session startup, common patterns, quick commands
2. **activeContext.md** - Current sprint, next actions, active work
3. **progress.md** - Overall status, recent completions, blockers

### Before Implementing a Feature
1. **systemPatterns.md** - Check for existing patterns to follow
2. **projectRules.md** - Review coding standards (error handling, testing, etc.)
3. **techContext.md** - Understand tech stack and dependencies

### Making an Architectural Decision
1. **decisions.md** - Review past decisions and rationale
2. **projectbrief.md** - Ensure alignment with vision and constraints
3. **systemPatterns.md** - Check for architectural patterns

### Debugging or Investigating
1. **quick-start.md** - Troubleshooting section, common issues
2. **techContext.md** - Technology-specific details
3. **tasks/YYYY-MM/** - Historical task documentation

---

## Document Descriptions

### projectbrief.md
**What**: Vision, goals, MVP scope, constraints, success criteria
**When to Read**: Complex tasks, major pivots, new contributor onboarding
**When to Update**: Major strategic shifts, milestone completion, constraint changes
**Key Sections**:
- Vision & Core Principles
- MVP Scope & Success Criteria
- Core Components
- Release Plan Milestones
- Constraints & Non-Negotiables

### systemPatterns.md
**What**: Architecture patterns, code organization, anti-patterns
**When to Read**: Before implementing features, architecture changes, code reviews
**When to Update**: Pattern discovery, new architectural approaches, pattern violations
**Key Sections**:
- Core Architecture
- WASM Execution Pattern
- Peer Discovery Pattern
- Job Lifecycle Pattern
- Security & Sandboxing
- Data Flow Patterns
- Error Handling
- Testing Patterns

### techContext.md
**What**: Tech stack, dependencies, build tools, deployment
**When to Read**: Session start, dependency questions, build/deployment issues
**When to Update**: New tech adoption, dependency changes, tooling updates
**Key Sections**:
- Technology Stack Overview
- Plasm Daemon (Rust dependencies)
- WASM Runtime (wasm3 vs wasmtime)
- Networking & Transport (libp2p)
- PHP Client SDK
- Build & Deployment
- Development Tools
- Future Technology Roadmap

### activeContext.md
**What**: Current sprint, active work, pending decisions, blockers
**When to Read**: Every session start, daily standup context
**When to Update**: Weekly, milestone shifts, blocker changes
**Key Sections**:
- Current Focus
- Current Sprint Backlog
- Upcoming Milestones
- Key Decisions This Week
- Blockers & Risks
- Recent Achievements
- Next Actions

### progress.md
**What**: Milestone status, task completion, metrics, timeline
**When to Read**: Session start, weekly reviews, progress reporting
**When to Update**: Major feature completion, milestone completion, weekly reviews
**Key Sections**:
- Release Milestones (status for each)
- Overall Progress (percentage complete)
- Recent Completions
- Active Work
- Blockers & Issues
- Key Metrics
- Timeline

### projectRules.md
**What**: Coding standards, naming conventions, error handling, testing, security
**When to Read**: Before writing code, during code review, when uncertain about conventions
**When to Update**: New patterns emerge, team consensus on standards
**Key Sections**:
- General Principles
- Rust Coding Standards
- PHP Coding Standards
- Error Handling
- Testing Standards
- Security Standards
- Documentation Standards
- Git & Version Control
- Code Review Checklist

### decisions.md
**What**: Architectural Decision Records (ADRs) - what, why, alternatives, consequences
**When to Read**: Before architectural changes, understanding past decisions
**When to Update**: Architectural decisions made, alternatives evaluated
**Key Sections**:
- Decision Log (chronological)
- Each decision: Status, Context, Decision, Alternatives, Consequences, References
- Superseded Decisions
- Future Decisions to Make

### quick-start.md
**What**: Session startup, common patterns, code snippets, troubleshooting, quick commands
**When to Read**: Session start, when stuck, looking for examples
**When to Update**: Weekly, new patterns discovered, common issues identified
**Key Sections**:
- Session Startup
- Common Patterns (WASM execution, peer discovery, error handling)
- File Locations
- Code Snippets (Rust, PHP)
- Troubleshooting
- Quick Commands
- Memory Bank Quick Lookup

---

## Task Documentation

### tasks/YYYY-MM/README.md
**What**: Monthly summary of completed tasks, patterns discovered, decisions made
**When to Read**: Investigating historical work, understanding context for legacy code
**When to Update**: End of month, major milestone completion
**Structure**:
```markdown
## Tasks Completed
- 2025-11-08: Task name (brief description)
  - See: [DDMMDD_task.md](./DDMMDD_task.md)

## Patterns Discovered
- Pattern name (link to systemPatterns.md)

## Decisions Made
- Decision (link to decisions.md)
```

### tasks/YYYY-MM/DDMMDD_task-name.md
**What**: Individual task documentation (objective, outcome, files modified, patterns applied)
**When to Read**: Understanding specific feature implementation, debugging related issues
**When to Create**: After task completion AND user approval
**Structure**:
```markdown
## Objective
What was accomplished

## Outcome
- Tests, coverage, build status
- Performance metrics

## Files Modified
- File paths with changes

## Patterns Applied
- Links to systemPatterns.md

## Integration Points
- How it connects to existing code

## Architectural Decisions
- Decisions made, rationale, trade-offs
```

---

## Current Task Documentation (2025-11)

**Status**: ✅ MVP Complete + Phase Boot Implemented

### Major Completions

| Task | Status | File |
|------|--------|------|
| Milestone 1: Local WASM Execution | ✅ DONE | [091109_milestone1_local_wasm_execution.md](./tasks/2025-11/091109_milestone1_local_wasm_execution.md) |
| Milestone 2: Peer Discovery | ✅ DONE | [251109_milestone2_peer_discovery.md](./tasks/2025-11/251109_milestone2_peer_discovery.md) |
| Milestone 3: Remote Execution | ✅ DONE | [091109_milestone3_remote_execution.md](./tasks/2025-11/091109_milestone3_remote_execution.md) |
| Milestone 4: Packaging & Demo | ✅ DONE | [091109_milestone4_packaging_demo.md](./tasks/2025-11/091109_milestone4_packaging_demo.md) |
| Library + Binary Refactor | ✅ DONE | [091109_library_binary_refactor.md](./tasks/2025-11/091109_library_binary_refactor.md) |
| Phase Boot (M1-M7) | ✅ DONE | [261126_phase_boot_implementation.md](./tasks/2025-11/261126_phase_boot_implementation.md) |

**Monthly Summary**: [tasks/2025-11/README.md](./tasks/2025-11/README.md)

---

## Memory Bank Workflow (AGENTS.md)

### When to Read Memory Bank Files

**Session Startup**:
- Fast Track: `tasks/YYYY-MM/README.md`, `quick-start.md`
- Standard: Current month README + core files (projectbrief, systemPatterns, techContext, activeContext, progress)
- Deep Dive: Standard + decisions.md + specific month README when investigating legacy

**During Development**:
- Before changes: systemPatterns.md, projectRules.md
- When stuck: quick-start.md, techContext.md
- Making decisions: decisions.md, projectbrief.md

### When to Update Memory Bank Files

**Rarely** (major changes only):
- projectbrief.md: Major pivots, scope changes
- techContext.md: New tech adoption, major dependency changes
- decisions.md: Architectural decisions made

**Regularly** (weekly/milestone):
- activeContext.md: Weekly updates, sprint changes
- progress.md: Weekly status, milestone completion
- quick-start.md: New patterns, common issues

**After Completion** (requires approval):
- tasks/YYYY-MM/DDMMDD_task.md: Individual task documentation
- tasks/YYYY-MM/README.md: Monthly summary
- systemPatterns.md: New patterns discovered
- projectRules.md: New coding standards

---

## Quick Reference: File Priority by Scenario

### Scenario: Starting New Feature
**Read**: systemPatterns.md → projectRules.md → activeContext.md
**Update After Completion**: tasks/YYYY-MM/DDMMDD_task.md + systemPatterns.md (if new pattern)

### Scenario: Fixing Bug
**Read**: quick-start.md (troubleshooting) → systemPatterns.md → relevant task docs
**Update After Completion**: tasks/YYYY-MM/DDMMDD_task.md (if significant fix)

### Scenario: Architectural Change
**Read**: decisions.md → projectbrief.md → systemPatterns.md
**Update After Completion**: decisions.md + systemPatterns.md + tasks/YYYY-MM/DDMMDD_task.md

### Scenario: Weekly Review
**Read**: progress.md → activeContext.md
**Update**: progress.md + activeContext.md

### Scenario: New Contributor Onboarding
**Read**: projectbrief.md → quick-start.md → systemPatterns.md → projectRules.md

---

## Memory Bank Statistics

**Total Files**: 9 core files + 6 major task docs + planning files
**Last Updated**: 2025-11-26
**Coverage**:
- ✅ Project vision and goals (projectbrief.md)
- ✅ Architecture patterns (systemPatterns.md) - updated with Phase Boot patterns
- ✅ Tech stack documentation (techContext.md)
- ✅ Current focus (activeContext.md) - updated for Phase Boot
- ✅ Progress tracking (progress.md) - MVP + Phase Boot complete
- ✅ Coding standards (projectRules.md)
- ✅ Architectural decisions (decisions.md)
- ✅ Quick reference (quick-start.md)
- ✅ Navigation (toc.md - this file)
- ✅ Monthly summaries (tasks/2025-11/README.md - complete)
- ✅ Phase Boot documentation (releases/boot/)

---

## External Documentation (Outside Memory Bank)

| File | Location | Purpose |
|------|----------|---------|
| README.md | `/Users/michael/Software/phase/` | Project overview for GitHub |
| CLAUDE.md | `/Users/michael/Software/phase/` | AGENTS.md workflow instructions |
| release_plan.yaml | `/Users/michael/Software/phase/` | Milestone planning and task breakdown |

---

**This TOC is your map. Update it when adding new Memory Bank files.**
