# docs/superpowers/

This directory is the **archive of big-feature implementation plans and design specs** — the artefacts used during large pushes, kept for historical reference.

## What's in here

- **`plans/`** — task-list style implementation plans with checkbox tasks, sequenced work, and file structure. These were used to sequence work on multi-week features (e.g. the Linux native UI rewrite in 2026-03). They reflect the state of the codebase **at the time they were written**; some tasks are stale, some are superseded. They are not authoritative now.

- **`specs/`** — per-platform UI design specs (Windows WinUI 3, Linux GTK4). These are the design rationale and feature map that informed the native UIs. Still useful as a reference for what each settings tab is supposed to do and why, but again: the code is authoritative.

## When to read these

- **You're working on a feature that matches one of these plans.** The plan may save you design time.
- **You're curious about why a platform's UI is shaped the way it is.** The specs explain.
- **You want to understand an old design decision.** The plans often capture why option A was chosen over option B.

## When NOT to read these

- As the source of truth for how the code currently works. That's the code itself, plus [`../ARCHITECTURE.md`](../ARCHITECTURE.md) and [`../dev/modules.md`](../dev/modules.md).
- As the source of truth for build instructions. That's [`../BUILD.md`](../BUILD.md).
- As the source of truth for any project rule. That's [`../../CLAUDE.md`](../../CLAUDE.md) + [`../dev/development-practices.md`](../dev/development-practices.md).

## Convention

New files here are named `YYYY-MM-DD-short-topic.md`. The date is the day the work kicked off. Don't edit old files after a feature ships — write a new one if the design changed significantly.
