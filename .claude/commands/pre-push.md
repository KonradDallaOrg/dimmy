---
description: Run the full pre-push checklist (fmt + clippy + tests) with the exact feature flags used in CI. Fails fast.
allowed-tools: Bash
---

Run these commands in sequence from the repo root. Stop at the first failure and report which step failed with the last ~20 lines of output.

1. `cd core && cargo fmt --check`
2. `cd core && cargo clippy --features local-stt,local-llm -- -D warnings`
3. `cd core && cargo test --lib --features local-stt,local-llm`

Then, only if `git diff --name-only origin/staging...HEAD` contains a path under `platforms/linux/`:

4. `cd platforms/linux && cargo clippy -- -D warnings`
5. `cd platforms/linux && cargo test`

Report format: the exact failing command and the last ~20 lines of its output, or "all green" on success. Nothing else.
