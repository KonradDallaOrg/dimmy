# Mac handover — Claude Desktop MCP bridge

**Date:** 2026-05-22
**Branch:** `feat/claude-mcp-bridge`
**Win status:** end-to-end working, validated 23:11 UTC with a real recap saved by Claude Desktop via MCP.
**Goal of this handover:** finish the Mac side from MacInCloud so the staging-tester DMG ships a working bridge on both platforms.

---

## Quick context (1 minute read)

The branch adds a two-way bridge between Dimmy and Claude Desktop:

- **Standalone Rust binary** `dimmy-mcp` (under `mcp-server/`) — stdio JSON-RPC 2.0 MCP server. 5 tools: `dimmy_get_recent_meetings`, `dimmy_get_meeting`, `dimmy_save_recap`, `dimmy_get_recent_dictations`, `dimmy_get_recap_template`.
- **DXT extension install** — Dimmy ships the binary, copies it at install time into the user's `Claude Extensions/dimmy/` dir alongside a `manifest.json` + `icon.png`. Same shape Anthropic's own PowerPoint connector uses. Auto-discovered by Claude Desktop on startup.
- **"Recap with Claude Desktop" button** on MeetingWindow Done view → opens `claude://claude.ai/new?q=<prompt>` deeplink. The prompt tells Claude to call `dimmy_get_recap_template` + `dimmy_get_meeting` + `dimmy_save_recap` to produce a structured recap. Claude follows, writes back to `recap.md`, Dimmy's UI picks it up.
- **Cross-provider title** — every recap (cloud LLM / local LLM / Claude CLI subscription / Claude Desktop MCP) starts with a `# Title` H1. `save_post_process` parses it + writes `meta.title` to meta.json. Sidebar + Done view show the LLM-chosen title. Click-to-edit on the Done view title to rename.

Three critical fixes uncovered during Win debugging that **also apply on Mac**:

1. **Keepalive notifications** every 30s prevent Claude Desktop from killing the server for idle (~5 min) and not respawning. Already in `mcp-server/src/main.rs`.
2. **No data-URI icon in `serverInfo`** — initialize response stays under 1 KB (was 70 KB with base64 PNG embedded). Caused Claude Desktop to stall on first tool call.
3. **Icon comes from real Claude app** (not hand-drawn SVG) — extracted from the install at runtime.

---

## What's already done on Mac (in this branch)

Files modified, ready to compile (untested — no Mac in this Win iteration):

- `platforms/macos/Dimmy/DimmyFFI.h` — declared 3 FFI entries: `dimmy_claude_desktop_status`, `_install`, `_uninstall`.
- `platforms/macos/Dimmy/Managers/DimmyCore.swift` — typed mirror `ClaudeDesktopStatus` + `installClaudeDesktopExtension(binaryPath:version:)` + `uninstallClaudeDesktopExtension()`.
- `platforms/macos/Dimmy/Views/Settings/ClaudeDesktopConnectSheet.swift` — 3-step wizard sheet (detect → install → wait for heartbeat). Uses `Bundle.main.bundlePath + "/Contents/Resources/dimmy-mcp"` for the binary source path.
- `platforms/macos/Dimmy/Views/Settings/MacIntegrationsPage.swift` — Claude Desktop status card with green check semantics matching Win.
- `platforms/macos/Dimmy.xcodeproj/project.pbxproj` — registered `ClaudeDesktopConnectSheet.swift` in build phases.

Rust core (`claude_desktop.rs`, `meeting.rs`, `ffi.rs`) is cross-platform — the same DLL/dylib serves both. `extensions_root()` resolves to `~/Library/Application Support/Claude/Claude Extensions/` on Mac via `dirs::config_dir()`.

---

## What's NOT done — your work for this session

### 1. Cross-compile `dimmy-mcp` for Mac + ship in `.app/Contents/Resources/` **[BLOCKER]**

Currently the Win build copies `dimmy-mcp.exe` next to `Dimmy.exe`. The Mac DMG build doesn't compile or copy the Mac binary. Without this the wizard fails at install with rc -2 ("binary not found").

**Where to touch:**

- `.github/workflows/release.yml` (prod tag `v*` non-staging)
- `.github/workflows/staging-tester.yml` (staging tag `v*-staging.N`)
- `.github/workflows/staging-auto-update.yml` (rolling staging push)

Add a build step BEFORE the "Build Rust" step (or as part of it):

```yaml
- name: Build dimmy-mcp for aarch64-apple-darwin
  working-directory: mcp-server
  run: cargo build --release --target aarch64-apple-darwin

- name: Copy dimmy-mcp into .app bundle
  run: |
    cp mcp-server/target/aarch64-apple-darwin/release/dimmy-mcp \
       platforms/macos/build/derived/Build/Products/Release/Dimmy.app/Contents/Resources/dimmy-mcp
    chmod +x platforms/macos/build/derived/Build/Products/Release/Dimmy.app/Contents/Resources/dimmy-mcp
    codesign --force --sign - \
       platforms/macos/build/derived/Build/Products/Release/Dimmy.app/Contents/Resources/dimmy-mcp
```

Important details:
- Adhoc sign with `-` so Gatekeeper doesn't block the spawn. Anthropic's PowerPoint extension's `index.js` is unsigned text, but a binary needs at least the ad-hoc signature.
- Copy step needs to land BEFORE the DMG creation (after xcodebuild produces the .app, before `create-dmg`).
- The Mac runner is `macos-15` (Apple Silicon arm64). `cargo` should already be on the runner via the `dtolnay/rust-toolchain@stable` step that already exists.

Smoke verification: after a green CI run, mount the produced DMG and check:
```bash
ls -la /Volumes/Dimmy*/Dimmy.app/Contents/Resources/dimmy-mcp
```
Should be ~3-4 MB executable.

### 2. Real Claude.app icon extraction (parity with Win) **[NICE-TO-HAVE]**

Win does this in `platforms/windows/Dimmy.Windows/Helpers/ClaudeIconExtractor.cs` — reads MSIX Assets dir, picks the biggest square logo, caches to `<config>/cache/claude-desktop-icon.png`.

Mac equivalent:

- Read `/Applications/Claude.app/Contents/Resources/AppIcon.icns`
- Convert to PNG via `sips -s format png in.icns --out out.png` (CLI shellout from Swift is fine) OR use `NSImage(contentsOf:).tiffRepresentation` → `NSBitmapImageRep` → `.representation(using: .png)`.
- Cache to `appState.configDirURL.appendingPathComponent("cache/claude-desktop-icon.png")`.
- Bind `MacIntegrationsPage`'s card `Image` to the cached file URL (instead of the current SF Symbol).

Without this the Mac card looks "off-brand" — a placeholder where Win shows the real Claude logo. Not a blocker but visibly inconsistent.

### 3. "Recap with Claude Desktop" button on MeetingView Mac **[NICE-TO-HAVE]**

Win has this on `platforms/windows/Dimmy.Windows/Views/MeetingWindow.xaml`:
```xml
<Button x:Name="RecapWithClaudeBtn" Click="RecapWithClaudeDesktop_Click"
        Visibility="Collapsed" ToolTipService.ToolTip="Recap with Claude Desktop (uses MCP)">
  <Image x:Name="RecapWithClaudeIcon" Width="16" Height="16"/>
</Button>
```

C# handler builds a deeplink and opens it:
```csharp
var deeplink = $"claude://claude.ai/new?q={System.Uri.EscapeDataString(prompt)}";
System.Diagnostics.Process.Start(new System.Diagnostics.ProcessStartInfo(deeplink) { UseShellExecute = true });
```

Mac equivalent: button on the MeetingView toolbar, visible only when `DimmyCore.shared.claudeDesktopStatus().extensionInstalled == true`. Click → `NSWorkspace.shared.open(URL(string: deeplink)!)`.

Prompt to embed (verbatim from Win — keep them in sync):
```
Recap Dimmy meeting `<ID>`.

1. Call `dimmy_get_recap_template` to fetch Dimmy's house format.
2. Call `dimmy_get_meeting` with id `<ID>` to read the transcript.
3. Produce a recap that follows the template's rules exactly (first line is a Markdown H1 title in the transcript's language).
4. Call `dimmy_save_recap` with id `<ID>` and your recap markdown to persist it back into Dimmy.
5. Confirm to me once saved.
```

### 4. SelfTests pin updates

`platforms/macos/Dimmy/Tests/SelfTests.swift` (or wherever the `runAtLaunch` assertions live) — if any pin counts a number that has changed (e.g. number of integrations cards, number of LLM presets, onboarding step count), update them. The MCP card adds one section to MacIntegrationsPage.

Run `scripts/dev/preflight-mac.sh` to catch all pinned-count drifts. This script also catches SwiftUI compile errors that this branch may have (untested from Win).

### 5. Verify the FOUR critical Mac-side details

After running `preflight-mac.sh`, manually test:

a. **Open Dimmy → Settings → Integrations**. Scroll to "Claude Desktop (MCP)" card. State should be either "Not connected" (clean) or "Installed" (if you've already done it).
b. **Click "Connect Claude Desktop"** → 3-step wizard. Detect should find `/Applications/Claude.app`. Install should write to `~/Library/Application Support/Claude/Claude Extensions/dimmy/` (4 files: manifest.json, icon.png, dimmy-mcp, Claude Extensions Settings/dimmy.json).
c. **Quit Claude Desktop fully** (⌘Q, not just close window). **Reopen Claude Desktop.** Open new chat. Type "List my last 2 Dimmy meetings."
d. **Watch the Claude MCP log** at `~/Library/Logs/Claude/mcp-server-dimmy.log` — should see:
   ```
   [info] Server started and connected successfully
   [info] Message from client: tools/list
   [info] Message from client: tools/call name=dimmy_get_recent_meetings
   [info] Message from server: notifications/message data=keepalive   ← every 30s
   ```
   The keepalive line proves the fix is in effect. If you DON'T see it the binary is the old one.

If everything passes, tag staging.7 with `git tag v0.6.50-staging.7 && git push --tags` to ship a Mac DMG for full validation.

---

## Build pipeline (for reference)

The staging-tester workflow already produces `Dimmy-Staging-macos-arm64.dmg`. After the dimmy-mcp build step lands:

1. Push tag `v0.6.50-staging.7` to GitHub
2. `staging-tester.yml` fires → ~15-20 min
3. Download DMG from https://github.com/KonradDallaOrg/dimmy/releases/tag/v0.6.50-staging.7
4. Mount, drag to Applications, launch Dimmy Staging.app

Note: staging.6 (cut alongside this handover) does NOT yet contain the Mac binary, so the wizard step 2 will fail there. Cut staging.7 only after the build pipeline change is in.

---

## File index for the impatient

| Concern | File |
|---|---|
| Rust core extension install/uninstall | `core/src/claude_desktop.rs` |
| FFI entries | `core/src/ffi.rs` (search `dimmy_claude_desktop_`) |
| Title parsing | `core/src/meeting.rs::parse_recap_title` |
| MCP server | `mcp-server/src/main.rs` (keepalive at line ~80) |
| MCP tools | `mcp-server/src/tools.rs` |
| Recap template | `mcp-server/templates/recap.md` |
| Mac FFI declarations | `platforms/macos/Dimmy/DimmyFFI.h` |
| Mac Swift wrappers | `platforms/macos/Dimmy/Managers/DimmyCore.swift` (line ~510+) |
| Mac wizard sheet | `platforms/macos/Dimmy/Views/Settings/ClaudeDesktopConnectSheet.swift` |
| Mac Settings card | `platforms/macos/Dimmy/Views/Settings/MacIntegrationsPage.swift` |
| Win wizard | `platforms/windows/Dimmy.Windows/Views/ClaudeDesktopConnectDialog.xaml{,.cs}` |
| Win MeetingWindow button | `platforms/windows/Dimmy.Windows/Views/MeetingWindow.xaml{,.cs}` (search `RecapWithClaude`) |
| Win icon extractor | `platforms/windows/Dimmy.Windows/Helpers/ClaudeIconExtractor.cs` |
| Win recap prompt | `platforms/windows/Dimmy.Windows/Helpers/MeetingRecapHelpers.cs::BuildStructuredRecapPrompt` |
| DMG bug fix (Applications symlink) | `.github/workflows/{release,staging-tester,staging-auto-update}.yml` |

---

## Known issues / follow-ups (not Mac-specific)

- 64×64 PNG variant of `icon.png` for inline tool-call rendering in Claude chat. Current 128×128 may downsample sub-optimally. Low priority.
- Explainer text in the Settings MCP card: "Claude Desktop respawns the MCP server on demand; the 'Server disconnected' message in Claude's developer panel is normal between calls." Pure UX polish.
- Telemetry event `recap.claude_desktop_launched` when the user clicks the button. Useful for measuring adoption.
- Verify cloud + local LLM recap paths also produce a `# Title` H1 (the prompt was updated, but Phi-4 or Groq might not honour it consistently). If not, add a regex in `save_post_process` that synthesizes a title from the meeting's transcript when the H1 is missing.

---

Good luck on the Mac side. Burn this doc into your terminal first thing — the keepalive + icon-bloat traps cost me ~3h of debugging in this branch; you don't need to repeat that.
