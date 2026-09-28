import XCTest
@testable import Dimmy

final class MeetingSpeakersTests: XCTestCase {
    private func dir(with json: String?) throws -> String {
        let d = FileManager.default.temporaryDirectory
            .appendingPathComponent("dimmy-speakers-\(UUID().uuidString)")
        try FileManager.default.createDirectory(at: d, withIntermediateDirectories: true)
        if let json {
            try json.write(to: d.appendingPathComponent("speakers.json"), atomically: true, encoding: .utf8)
        }
        return d.path
    }

    func testLoadsSpeakersInFileOrderWithTheirSlots() throws {
        let d = try dir(with: """
        [{"id":"S1","name":"Speaker 1","band":"system","talk_secs":97.5,"segments":[[0.0,4.2],[9.0,12.5]]},
         {"id":"S2","name":"Marco","band":"mic","talk_secs":12,"segments":[[4.2,9.0]]}]
        """)
        let s = MeetingSpeakers.load(dir: d)
        XCTAssertEqual(s.map(\.name), ["Speaker 1", "Marco"])
        XCTAssertEqual(s.map(\.colorIndex), [0, 1])
        XCTAssertEqual(s[0].segments.count, 2)
        XCTAssertEqual(s[0].segments[1].end, 12.5)
        XCTAssertEqual(s[1].band, "mic")
        XCTAssertEqual(MeetingSpeakers.colorsByName(s)["marco"], 1)
    }

    func testAnUndiarizedMeetingHasNoSpeakers() throws {
        XCTAssertTrue(MeetingSpeakers.load(dir: try dir(with: nil)).isEmpty)
        XCTAssertTrue(MeetingSpeakers.load(dir: try dir(with: "not json")).isEmpty)
        XCTAssertTrue(MeetingSpeakers.load(dir: nil).isEmpty)
    }

    func testTalkTimeFormat() {
        XCTAssertEqual(MeetingSpeakers.formatTalkTime(97), "1:37")
        XCTAssertEqual(MeetingSpeakers.formatTalkTime(3725), "1:02:05")
        XCTAssertEqual(MeetingSpeakers.formatTalkTime(-3), "0:00")
    }

    func testTracksAreNotPeople() {
        XCTAssertTrue(MeetingSpeakers.isTrack("MIC"))
        XCTAssertTrue(MeetingSpeakers.isTrack("system"))
        XCTAssertFalse(MeetingSpeakers.isTrack("Speaker 1"))
    }
}
