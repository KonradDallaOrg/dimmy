# Handoff — Mac side for Phase 7 + 8 (icons, drop, waveform, cloud file-load, captions)

> **Source:** Win-side complete on `feat/v2-unified`, last commit
> `0beb006 fix(stt/win): preserve alpha when saving extracted icons`.
> Rust core is fully cross-platform; Swift just needs the equivalent
> wrappers per feature.
>
> Companion doc: `2026-05-06-mac-v2-features.md` covers Phases 1–6
> (rules / history-v2 schema / file-load / meeting). This doc covers
> the new stuff layered on top.

## TL;DR — Mac parity checklist

| # | Feature | Win mechanism | Mac equivalent |
|---|---------|---------------|----------------|
| 7.1 | Real app icons | `SHGetFileInfo` + GDI+ | `NSWorkspace.icon(forFile:)` |
| 7.2 | File picker (offline transcribe) | `IFileOpenDialog` COM | `NSOpenPanel` |
| 7.3 | Waveform render in History detail | `Canvas` + `WavPeaks.cs` | `Path` + the same `WavPeaks` translated to Swift, OR use `AVAudioFile` peaks |
| 7.3 | Audio playback in History detail | `MediaPlayerElement` | `AVPlayerView` from AVKit |
| 7.4 | Word timestamps from Parakeet | already in Rust | bind same FFI; UI for word-highlight TBD |
| 8.1 | Transparent icons via `IShellItemImageFactory` | win-only | `NSWorkspace.icon(forFile:)` returns `NSImage` with alpha already preserved — done |
| 8.2 | Clickable waveform → seek | Canvas `PointerPressed` → `MediaPlayer.PlaybackSession.Position` | `NSGestureRecognizer` on the path view → `AVPlayer.seek(to:)` |
| 8.3 | Drag-drop file-load | `WM_DROPFILES` + UIPI bypass | `NSDragDestination` — way simpler, no IL/UIPI on Mac |
| 8.4 | Live meeting captions | poll `transcripts.txt` every 2 s | same — file-format agnostic |
| 8.5 | Icon resolution 256×256 | `IShellItemImageFactory.GetImage(SIZE 256, BIGGERSIZEOK)` | `NSWorkspace.icon` → `image.bestRepresentation(forSize: 256)` |
| 8.6 | Preserve alpha when caching icons | `GetDIBits` → `Format32bppArgb` | `NSImage` already preserves alpha when written via `NSBitmapImageRep.representation(.png)` |
| 8.x | Cloud STT for file-load | `dimmy_transcribe_file` extended in Rust | already cross-platform; just wire the Swift caller to surface new rc codes |
| 8.x | "Long file" confirmation dialog | `ContentDialog` with WAV-header peek | `NSAlert` + same `WavPeaks` peek logic |

---

## 7.1 + 8.1 + 8.5 + 8.6 — App icons

### Rust side
Nothing changed — icons are pure host concern.

### What the Win side does (for reference)
- `Helpers/IconExtractor.cs` extracts the running .exe's icon via
  `IShellItemImageFactory.GetImage(SIZE 256, BIGGERSIZEOK | RESIZETOFIT)`
- Pulls the BGRA pixels via `GetDIBits` (top-down, BI_RGB) so alpha
  is preserved — `GdipCreateBitmapFromHBITMAP` would have flattened
  it to opaque black
- Saves PNG to `%LOCALAPPDATA%\Dimmy\app-icons\<process-stem>.png`
- Cache version sentinel in `.cache-version` so algorithm bumps
  invalidate stale PNGs
- Triggered:
  - On every hotkey press for the foreground app
    (App.xaml.cs::CaptureAndPushAppContext)
  - Warm-up scan over `Process.GetProcesses()` when Settings opens

### What Mac needs

```swift
// Helpers/IconExtractor.swift
import AppKit

enum IconExtractor {
    static let cacheDir: URL = {
        let base = FileManager.default.urls(for: .cachesDirectory,
            in: .userDomainMask).first!
        let dir = base.appendingPathComponent("Dimmy/app-icons")
        try? FileManager.default.createDirectory(at: dir,
            withIntermediateDirectories: true)
        return dir
    }()

    /// "/Applications/Slack.app" → "<cache>/com.tinyspeck.slackmacgap.png"
    static func ensureCached(forBundlePath path: String) {
        guard let bundle = Bundle(path: path),
              let bid = bundle.bundleIdentifier else { return }
        let pngURL = cacheDir.appendingPathComponent("\(bid).png")
        if FileManager.default.fileExists(atPath: pngURL.path) { return }

        let icon = NSWorkspace.shared.icon(forFile: path)
        // 128×128 is the largest representation Finder uses; matches
        // Win's 256×256 jumbo since Mac apps are vector-ICNS.
        icon.size = NSSize(width: 128, height: 128)
        guard let tiff = icon.tiffRepresentation,
              let rep = NSBitmapImageRep(data: tiff),
              let png = rep.representation(using: .png, properties: [:])
        else { return }
        try? png.write(to: pngURL)
    }

    static func cachedURL(forBundleId bid: String) -> URL? {
        let url = cacheDir.appendingPathComponent("\(bid).png")
        return FileManager.default.fileExists(atPath: url.path) ? url : nil
    }
}
```

Call `ensureCached` from the same hook that pushes app-context to
Rust (foreground-app capture on hotkey). Settings-side rule list
binds `Image(nsImage: NSImage(contentsOf: cachedURL))` per row.

NSWorkspace already returns icons with alpha; no GDI+ tricks needed.

---

## 7.2 — File picker (offline transcribe)

### What Mac needs

```swift
import AppKit

func pickWavFile() async -> URL? {
    let panel = NSOpenPanel()
    panel.allowedContentTypes = [.audio]  // or [.wav] specifically
    panel.allowsMultipleSelection = false
    panel.canChooseDirectories = false
    panel.title = "Pick a WAV to transcribe"
    return await withCheckedContinuation { cont in
        panel.begin { resp in
            cont.resume(returning: resp == .OK ? panel.url : nil)
        }
    }
}
```

Call from the Settings → "TRANSCRIBE A FILE" card → "Pick file…" button.
Then call `dimmy_transcribe_file(path, buf, len)` with the picked path.

---

## 7.3 + 8.2 — History detail panel: waveform + playback + scrub

### Win XAML (for reference)
```xml
<Canvas x:Name="HistoryWaveformCanvas"
        Background="Transparent"
        PointerPressed="HistoryWaveformCanvas_PointerPressed" />
<MediaPlayerElement x:Name="HistoryAudioPlayer"
                    AreTransportControlsEnabled="True"
                    MaxHeight="110" />
```

### What Mac needs

SwiftUI version of the History detail panel:

```swift
import SwiftUI
import AVKit

struct HistoryDetailView: View {
    @ObservedObject var item: HistoryItem
    @State private var peaks: [Float] = []
    @State private var player = AVPlayer()
    @State private var playheadFrac: Double = 0

    var body: some View {
        VStack(alignment: .leading, spacing: 8) {
            Text("Raw transcript").font(.caption).foregroundColor(.secondary)
            Text(item.text).textSelection(.enabled)
            // …enhanced text, app/lang/duration strip, copy/delete buttons…

            if let path = item.audioPath, FileManager.default.fileExists(atPath: path) {
                WaveformView(peaks: peaks, playheadFrac: playheadFrac)
                    .frame(height: 64)
                    .onTapGesture { loc in
                        let frac = loc.x / geometryWidth
                        let total = player.currentItem?.duration.seconds ?? 0
                        player.seek(to: CMTime(seconds: total * frac, preferredTimescale: 600))
                        playheadFrac = frac
                    }
                VideoPlayer(player: player).frame(height: 50)
            }
        }
        .task(id: item.id) {
            // Read peaks on a background queue
            peaks = await Task.detached {
                WavPeaks.readPeaks(path: item.audioPath, bucketCount: 200)
            }.value
            if let path = item.audioPath {
                player.replaceCurrentItem(with: AVPlayerItem(url: URL(fileURLWithPath: path)))
                // Periodic time observer for the playhead
                player.addPeriodicTimeObserver(forInterval: CMTime(seconds: 0.05,
                    preferredTimescale: 600), queue: .main) { time in
                    let total = player.currentItem?.duration.seconds ?? 0
                    if total > 0 { playheadFrac = time.seconds / total }
                }
            }
        }
    }
}

struct WaveformView: View {
    let peaks: [Float]
    let playheadFrac: Double
    var body: some View {
        Canvas { ctx, size in
            let bar = size.width / CGFloat(peaks.count)
            for (i, p) in peaks.enumerated() {
                let h = max(1, CGFloat(p) * (size.height - 2))
                let r = CGRect(x: CGFloat(i) * bar, y: (size.height - h) / 2,
                               width: max(1, bar - 1), height: h)
                ctx.fill(Path(roundedRect: r, cornerSize: CGSize(width: 1, height: 1)),
                         with: .color(.blue))
            }
            // Playhead
            let x = size.width * playheadFrac
            ctx.fill(Path(CGRect(x: x, y: 0, width: 2, height: size.height)),
                     with: .color(.orange))
        }
    }
}
```

`WavPeaks.swift` is a straight port of `Helpers/WavPeaks.cs` — RIFF
chunk walker + per-bucket peak aggregation. ~80 lines, no
dependencies.

---

## 7.4 — Word timestamps (Parakeet)

### Already done in Rust
- `core/src/parakeet.rs::transcribe_with_word_timestamps(pcm) -> (text, json)`
- `core/src/transcribe.rs::transcribe_audio_local_parakeet_with_word_ts`
- `dimmy_transcribe_file` accumulates per-chunk timestamps with
  offset, saves via `dimmy_history_update_word_timestamps`
- macOS-aarch64 FluidAudio path returns `(text, "[]")` — no
  timestamps yet (FluidAudio's API would need a separate hook).

### What Mac UI needs (optional)
History detail panel could highlight the active word in the
transcript as the user plays back. Bind to AVPlayer's periodic
time observer: find the word whose `start <= currentSec < end` and
underline / bold it. Stretch goal — not blocking initial Mac ship.

---

## 8.3 — Drag-drop

### Win (for reference)
- `WM_DROPFILES` legacy Win32 path (subclassing all 4 child HWNDs:
  `WinUIDesktopWin32WindowClass`, `InputNonClientPointerSource`,
  `Microsoft.UI.Content.DesktopChildSiteBridge`, `InputSiteWindowClass`)
- `ChangeWindowMessageFilterEx` on each HWND to bypass UIPI when
  Dimmy runs at higher integrity level than Explorer (which it
  does when launched from an elevated shell — observed during dev)
- Belt-and-suspenders: parallel XAML root-grid `AllowDrop=true`
  with `AddHandler(handledEventsToo: true)` — works once UIPI is
  cleared

### What Mac needs
**Way simpler** — no IL/UIPI:

```swift
struct FileLoadDropTarget: View {
    @State private var droppedURL: URL?
    var body: some View {
        VStack { /* card content */ }
            .onDrop(of: [.fileURL], isTargeted: nil) { providers in
                guard let p = providers.first else { return false }
                _ = p.loadObject(ofClass: URL.self) { url, _ in
                    if let url, url.pathExtension.lowercased() == "wav" {
                        Task { await transcribeFile(url.path) }
                    }
                }
                return true
            }
    }
}
```

That's it. AppKit/SwiftUI handles drag/drop natively without any of
the Win shims.

---

## 8.x — Cloud STT for file-load

### Already done in Rust
`dimmy_transcribe_file` now branches on `stt_mode`:
- `"local"` → existing chunked Parakeet/Whisper path
- anything else → new cloud branch builds a tokio runtime, calls
  `transcribe_chunked` with the provider's `max_file_bytes`

New return codes:
- `-6` cloud config incomplete (URL/key/model missing)
- `-7` tokio runtime failed
- `-8` cloud transcribe failed

### What Mac needs
Just translate the rc codes to user-facing messages in the Swift
caller. No new FFI surface.

---

## 8.x — "Long file" confirmation dialog

### Win logic (for reference)
- `PeekWavMetrics(path)` reads only the RIFF/fmt header → duration + size
- `ConfirmLargeFileAsync` shows a `ContentDialog` when:
  - duration ≥ 5 min OR size ≥ 50 MB (any backend)
  - OR cloud + duration ≥ 1 min (cost awareness)
- Cancel returns "Cancelled" status

### What Mac needs

```swift
func confirmLargeFile(path: String) async -> Bool {
    let (durationSec, sizeBytes) = WavPeaks.peekMetrics(path: path)
    let isCloud = AppConfig.shared.sttMode != "local"
    let large = durationSec >= 300 || sizeBytes >= 50 * 1024 * 1024
    if !large && !isCloud { return true }
    if !large && isCloud && durationSec < 60 { return true }

    let mins = Int((durationSec / 60).rounded())
    let sizeMB = Double(sizeBytes) / (1024 * 1024)
    let costHint = isCloud
        ? "\nThis will be sent to your cloud provider — billing applies."
        : "\nThis runs locally and may take a few minutes."

    let alert = NSAlert()
    alert.messageText = "Long file"
    alert.informativeText = "\(URL(fileURLWithPath: path).lastPathComponent)\n" +
        "≈ \(mins) min · \(String(format: "%.1f", sizeMB)) MB\(costHint)\n\nProceed?"
    alert.addButton(withTitle: "Transcribe")
    alert.addButton(withTitle: "Cancel")
    return alert.runModal() == .alertFirstButtonReturn
}
```

---

## 8.4 — Live meeting captions

### Win (for reference)
- `MeetingWindow.xaml.cs::OnPollTick` — polls `transcripts.txt` every 2 s
- Caches `_lastTranscriptLen` so we only refresh the TextBlock when
  the file actually grew
- `FileShare.ReadWrite` on the FileStream so concurrent Rust
  appends don't trigger IOException
- `TranscriptScroll.ChangeView(null, double.MaxValue, null, true)` to
  auto-scroll to bottom on each update

### What Mac needs

```swift
@MainActor
class MeetingViewModel: ObservableObject {
    @Published var transcript = "🎙 Listening… first transcript appears in ~15 s."
    @Published var elapsed = "00:00:00"
    private var timer: Timer?
    private var lastLen: Int = -1
    private var startedAt = Date()

    func start() {
        // ... call dimmy_meeting_start ...
        startedAt = Date()
        lastLen = -1
        timer = Timer.scheduledTimer(withTimeInterval: 2, repeats: true) { [weak self] _ in
            Task { @MainActor in self?.poll() }
        }
    }

    private func poll() {
        elapsed = formatElapsed(Date().timeIntervalSince(startedAt))
        let meetings = FileManager.default.urls(for: .applicationSupportDirectory,
            in: .userDomainMask)[0].appendingPathComponent("dimmy/meetings")
        guard let dirs = try? FileManager.default.contentsOfDirectory(at: meetings,
            includingPropertiesForKeys: [.contentModificationDateKey]),
            let latest = dirs.sorted(by: { (a, b) -> Bool in
                (try? a.resourceValues(forKeys: [.contentModificationDateKey])
                    .contentModificationDate ?? .distantPast)!
                > (try? b.resourceValues(forKeys: [.contentModificationDateKey])
                    .contentModificationDate ?? .distantPast)!
            }).first else { return }
        let txt = latest.appendingPathComponent("transcripts.txt")
        guard let attrs = try? FileManager.default.attributesOfItem(atPath: txt.path),
              let size = attrs[.size] as? Int, size != lastLen,
              let content = try? String(contentsOf: txt) else { return }
        lastLen = size
        transcript = content
    }
}
```

---

## How to start on Mac

```bash
git fetch origin
git checkout staging          # tip is the v2 + Phase 7-8 cherry-picks
cd platforms/macos
# Build/run the existing macOS app — Phase 1-6 should already work
# (companion handoff doc covers those)

# Phase 7+8 work order (suggested):
#  1. IconExtractor.swift          (4-6 h)
#  2. NSOpenPanel for file picker  (15 min)
#  3. Drag-drop on file-load card  (30 min)
#  4. WavPeaks.swift port          (1-2 h)
#  5. Waveform view + AVPlayer     (3-4 h)
#  6. Long-file NSAlert            (30 min)
#  7. Cloud rc codes translation   (15 min)
#  8. Live meeting captions poll   (1-2 h)
```

Total: roughly **1-1.5 days** of Swift work to reach Win parity on
Phase 7+8. The Rust core is unchanged — every feature has been
verified to build with the existing macos targets in CI.

---

## Verification

Before merging Mac changes back to staging, run from `core/`:

```bash
cargo fmt --check
cargo clippy --features local-stt,local-llm -- -D warnings
cargo test --lib --features local-stt,local-llm
```

The Win-side build commands stay unchanged — see
`docs/dev/windows-ci.md`.
