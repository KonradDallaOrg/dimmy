---
description: Release-time telemetry coverage audit (automation Layer 3). Diffs commits since the last release tag, finds user-facing features with no PostHog event, and PROPOSES privacy-safe events for approval. Run before each staging.N / rcN.
allowed-tools: Bash, Read, Grep
---

You are the judgment half of Dimmy's telemetry coverage system. Layer 2
(`core/tests/telemetry_coverage.rs`) already guarantees no dead/undocumented
`Event` variants; your job is the part a test cannot decide: **did a new
user-facing feature ship without a metric, and does it deserve one?**

Privacy is non-negotiable (CLAUDE.md "Telemetry — privacy hard rules"): propose
ONLY categorical enums, counts, durations, error categories. NEVER transcript /
prompt text, file paths, hostnames, device/mic names, usernames, IP.

Steps:

1. **Find the baseline.** `git tag --sort=-version:refname | head -3` and pick
   the most recent release tag. Set `RANGE=<lasttag>..HEAD`.

2. **List what shipped.** `git log --oneline <RANGE>` and
   `git diff --name-only <RANGE>`. Focus on user-facing surfaces:
   - new `pub extern "C" fn dimmy_*` in `core/src/ffi.rs` (entry points for user actions),
   - new modules in `core/src/` (a new capability),
   - new config-driven behavior, new Settings toggles, new pill/meeting/host commands.

3. **Read the source of truth.** Read the "Coverage map" section of
   `docs/dev/telemetry-implementation.md` (live variants, reserved, and the
   TODO/skip lists). Anything already `live`, `TODO`, or in `skip` is NOT a new
   finding — do not re-report it.

4. **Cross-reference.** For each user-facing surface from step 2, check whether
   it emits an event (grep `core/src` for `telemetry::track` / `Event::` near
   the feature, and the host bridge arms in `dimmy_telemetry_track_typed`). A
   surface with no event AND not in the map's TODO/skip list is a GAP.

5. **Decide + propose.** For each gap, judge: is it decision-useful pre-launch
   (funnel, adoption, failure rate, drop-off)? If yes, propose:
   - event name (snake.dotted, matching existing style),
   - a new `Event` variant name (CamelCase) OR "reuse `<Existing>` + prop",
   - minimal privacy-safe props,
   - one-line rationale.
   If a surface is genuinely not worth tracking, propose adding it to the map's
   **skip** list with a reason (so it stops surfacing).

Output: a short ranked markdown list — `must` vs `nice` — of proposed events
(or skip-list additions), each with variant, props, and rationale. Do NOT write
code; this is a proposal for the human to approve. End by reminding: approved
events follow the 6-step "Adding a new event" process in
`docs/dev/telemetry-implementation.md` and must be added to the coverage map
(or Layer 2 / the privacy test will fail).
