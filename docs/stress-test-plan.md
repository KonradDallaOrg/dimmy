# Dimmy Stress Testing & Assertions Plan

> **For agentic workers:** Use superpowers:subagent-driven-development or superpowers:executing-plans to implement this plan.

**Goal:** Fill production code with debug_assert! invariants, then build offline stress/fuzz tests that simulate extreme scenarios (1h recordings, 1000+ chunks, NaN/Inf audio, OOM boundaries) to find crashes.

**Architecture:** New `src-tauri/src/stress_tests.rs` integration test file with test harness that simulates audio buffers, chunk splitting, preprocessing pipeline, and WAV encoding at scale — all offline, no API calls. Assertions go directly into production code (`audio.rs`, `preprocess.rs`, `llm.rs`, `lib.rs`).

**Tech Stack:** Rust std test harness, `#[cfg(test)]` modules, `debug_assert!`, synthetic audio generation (sine waves, noise, silence, edge values).

---

## Phase A: Production Assertions

Add debug_assert! throughout production code to catch invariant violations early.

## Phase B: Stress Test Suite

Offline tests simulating extreme scenarios — runs for hours, no API needed.

## Phase C: (Future — with user) E2E tests with real providers.
