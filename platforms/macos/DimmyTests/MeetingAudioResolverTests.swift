import XCTest

@testable import Dimmy

// MARK: - MeetingAudioResolverTests
//
// Pins `MeetingViewModel.resolveMeetingAudio(dir:base:)` — the single
// chokepoint every Mac UI path (Done-view playback URLs, regenerate-
// transcript, mtime sort) hits to pick `audio.ogg` or fall back to
// `audio.wav`. A regression here would silently break BOTH older
// meetings (WAV) and new ones (Ogg) — the .ogg branch lands users on
// a missing-file UX even when their mix track is there.

final class MeetingAudioResolverTests: XCTestCase {
    private var tmp: URL!

    override func setUpWithError() throws {
        tmp = FileManager.default.temporaryDirectory
            .appendingPathComponent("dimmy_resolver_\(UUID().uuidString)")
        try FileManager.default.createDirectory(at: tmp, withIntermediateDirectories: true)
    }

    override func tearDownWithError() throws {
        try? FileManager.default.removeItem(at: tmp)
    }

    private func touch(_ name: String) throws {
        try Data().write(to: tmp.appendingPathComponent(name))
    }

    func testOggPreferredOverWav() throws {
        // The new + the old format coexist (e.g. an in-flight Mac that
        // re-recorded after the gate flip, or a copy/paste from disk).
        // Pick the newer format — the user expects the compact track.
        try touch("audio.ogg")
        try touch("audio.wav")
        let url = MeetingViewModel.resolveMeetingAudio(dir: tmp.path, base: "audio")
        XCTAssertEqual(url?.pathExtension, "ogg")
    }

    func testFallsBackToWavWhenOggMissing() throws {
        // Older Mac meetings (pre-gate-flip) only have .wav. Must still
        // play / regenerate / sort by mtime — the fallback is what
        // keeps the existing library functional.
        try touch("audio.wav")
        let url = MeetingViewModel.resolveMeetingAudio(dir: tmp.path, base: "audio")
        XCTAssertEqual(url?.pathExtension, "wav")
    }

    func testReturnsOggWhenOnlyOggPresent() throws {
        // Post-gate-flip meetings only have .ogg. Resolver must NOT
        // return a non-existent .wav URL just because it's the legacy
        // default — that would land the playback bar on a phantom file
        // and silently show "no waveform".
        try touch("audio.ogg")
        let url = MeetingViewModel.resolveMeetingAudio(dir: tmp.path, base: "audio")
        XCTAssertEqual(url?.pathExtension, "ogg")
    }

    func testReturnsNilWhenNeitherPresent() throws {
        // Meeting dir without a mix track yet (race on the very first
        // chunk write, or a corrupted meeting). nil signals "no audio"
        // so the playback bar collapses + the regenerate path toasts.
        let url = MeetingViewModel.resolveMeetingAudio(dir: tmp.path, base: "audio")
        XCTAssertNil(url)
    }

    func testPerTrackBasesResolveIndependently() throws {
        // Each track is resolved on its own — a meeting that has
        // `audio.ogg` + `audio_mic.wav` + nothing for system must return
        // the matching extension per track AND nil for the absent one.
        // Real-world shape: post-gate-flip mix is .ogg but a per-track
        // WAV re-encoder hasn't caught up.
        try touch("audio.ogg")
        try touch("audio_mic.wav")
        let mix = MeetingViewModel.resolveMeetingAudio(dir: tmp.path, base: "audio")
        let mic = MeetingViewModel.resolveMeetingAudio(dir: tmp.path, base: "audio_mic")
        let sys = MeetingViewModel.resolveMeetingAudio(dir: tmp.path, base: "audio_system")
        XCTAssertEqual(mix?.pathExtension, "ogg")
        XCTAssertEqual(mic?.pathExtension, "wav")
        XCTAssertNil(sys)
    }

    func testUnrelatedFilesInDirAreIgnored() throws {
        // Don't get confused by sibling files (notes.md, transcripts.txt,
        // peaks.json caches). Only `<base>.ogg` and `<base>.wav` count.
        try touch("notes.md")
        try touch("audio.wav.peaks.json")
        try touch("transcripts.txt")
        let url = MeetingViewModel.resolveMeetingAudio(dir: tmp.path, base: "audio")
        XCTAssertNil(url)
    }
}
