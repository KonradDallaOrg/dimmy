# Handoff — 2026-06-14 — meeting-type recap + folder export + combo removal

Three features finished end-to-end (Win built + 341 xUnit green; Mac code
written, validated by CI). Branch: `staging`. NOT yet committed / tagged.

## ✅ DONE — 1. Meeting-type override dropdown (the missing half of #277)
Recap auto-detects type AND the user can force one, then regenerate.
- Taxonomy unchanged (Auto + 8 + General), 11-section contract untouched —
  type is a prompt hint that round-trips via `<!-- dimmy-type: KEY -->`.
- **Win**: `RecapTypePicker` ComboBox in the Done-view toolbar
  (`MeetingWindow.xaml`), bound in ctor to `MeetingRecapHelpers.MeetingTypes`,
  default Auto. `RegenerateRecap_Click` passes the picked key (auto→"") into
  `GeneratePostProcessAsync(dir, transcript, meetingType)`. `ApplyDoneSections`
  reflects the resolved `__TYPE__` back into the picker. Chip still shows result.
- **Mac**: `Picker($vm.selectedMeetingType)` in `MeetingDoneView` toolbar;
  `MeetingViewModel.doneSections.didSet` reflects `__TYPE__` → picker;
  `regenerateRecap()` + `runPostProcess()` pass it into `runRecap(...meetingType:)`.

## ✅ DONE — 2. Export recaps to a folder (#278, Obsidian / Drive / Dropbox)
After each recap save, copy `recap.md` → `<folder>/<title> (<meeting-id>).md`.
Free cloud + notes sync, NO OAuth. The meeting-id suffix = regenerate
overwrites the same file, different meetings never collide.
- Pure, unit-tested filename sanitizer:
  `MeetingRecapHelpers.SanitizeRecapFileName` (Win, 8 new xUnit tests) /
  `MeetingPostProcessService.sanitizeRecapFileName` (Mac mirror).
- **Win**: `UiPreferences.RecapExportFolder` + `Services/RecapExportService.cs`
  (`TryExport`, best-effort, never throws) called from BOTH recap paths
  (`MeetingPostProcessService.RunRecapAsync` + `MeetingWindow.GeneratePostProcessAsync`).
  Settings card "Export recaps to a folder" in `SettingsWindow.xaml` (Browse /
  Turn off) + handlers in `SettingsWindow.xaml.cs`.
- **Mac**: `MeetingPostProcessService.tryExportRecap` (UserDefaults key
  `recapExportFolder`) called after `meetingSavePostProcess`. Settings
  `recapExportGroup` in `MacOutputPage` (Advanced) with `@AppStorage` + NSOpenPanel.

## ✅ DONE — 3. Removed the Mix/Voice/System playback combo (Win-only)
The half-wired `DoneTrackSelector` (switched audio source but never redrew the
waveform) is GONE: ComboBox removed from `MeetingWindow.xaml` (Grid 3→2 rows),
`DoneTrackSelector_SelectionChanged` + `_doneMix/Mic/SystemPath` fields +
wiring removed from `.cs`. Playback always uses the full mix; the mirrored
waveform (mic+system cached peaks) is unchanged. Mac never had this combo.

## Verify status
- Win: built clean (`-p:Platform=x64`, 0 errors), 341/341 xUnit green, app
  relaunched 17:50 for user check.
- Mac: code written + mirrored; NOT compiled locally (no xcodebuild on Win) —
  relies on staging-tester CI. Watch the first Mac compile of these Swift edits.

## Next (not started)
- Commit + push to staging, then version train (staging.N → rc → stable) per
  CLAUDE.md version-check rule (`gh release list` + `git tag` + Cargo.toml first).
- #279 Google Drive native-Doc via OAuth — phase-2 optional (folder-export
  already covers Drive-sync for free). DON'T BUILD Antigravity (ToS ban).
