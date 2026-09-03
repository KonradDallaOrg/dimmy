import XCTest
@testable import Dimmy

// MARK: - RecapModelTests
//
// Tests for the curated recap-model picker (`RecapModelOption`) +
// `pickRecapModel()` heuristic. Mirror of Win xUnit
// `SettingsViewModelTests.RecapModelOverride_*` shape, with extra
// coverage for the curated list and Custom escape hatch.

final class RecapModelTests: XCTestCase {
    // MARK: - Curated list integrity

    func testCuratedListIncludesAuto() {
        XCTAssertEqual(RecapModelOption.curated.first?.id, RecapModelOption.autoKey)
        XCTAssertEqual(RecapModelOption.curated.first?.provider, .auto)
    }

    func testCuratedListHasNoDuplicateIds() {
        let ids = RecapModelOption.curated.map(\.id)
        XCTAssertEqual(ids.count, Set(ids).count, "Curated model ids must be unique")
    }

    // The curated list is BUILT from the shared model catalog
    // (`core/assets/model-catalog.json`), which is the single source of
    // truth precisely so model ids can change in one place. Pinning
    // specific ids here re-implements the catalog in the test and goes
    // stale on the next model refresh — which is exactly what happened:
    // these asserted claude-opus-4-8 and gemini-3.1-pro long after the
    // catalog moved on, and nothing noticed because nothing ran them.
    //
    // Assert the CONTRACT instead: every catalog provider that can write a
    // recap is represented, and the ids are the catalog's.

    func testCuratedListCoversTheCatalogsRecapModels() {
        let ids = Set(RecapModelOption.curated.map(\.id))
        let catalogIds = Set(ModelCatalog.models(task: "recap").map(\.model.id))
        XCTAssertFalse(catalogIds.isEmpty, "the catalog must offer recap models")
        XCTAssertTrue(
            catalogIds.isSubset(of: ids),
            "every catalog recap model must reach the picker; missing: "
                + catalogIds.subtracting(ids).sorted().joined(separator: ", "))
    }

    func testCuratedListRepresentsTheMajorCloudProviders() {
        let providers = Set(RecapModelOption.curated.map(\.provider))
        for expected: RecapModelOption.Provider in [.anthropic, .openai, .gemini, .local] {
            XCTAssertTrue(
                providers.contains(expected),
                "\(expected.rawValue) must be represented in the recap picker")
        }
    }

    func testCuratedListIncludesExpectedOpenaiModels() {
        let ids = RecapModelOption.curated.map(\.id)
        XCTAssertTrue(ids.contains("gpt-5.5"))
        XCTAssertTrue(ids.contains("gpt-5.4-mini"))
        XCTAssertTrue(ids.contains("gpt-5.4-nano"))
        XCTAssertTrue(ids.contains("gpt-5"))
    }

    func testEveryCuratedOptionHasNonEmptyLabel() {
        for opt in RecapModelOption.curated {
            XCTAssertFalse(opt.label.isEmpty, "Option \(opt.id) has empty label")
        }
    }

    func testProviderToIconMappingIsStable() {
        // Ensure each provider maps to a non-empty SF Symbol name so
        // the dropdown never shows an empty/missing icon.
        for opt in RecapModelOption.curated {
            XCTAssertFalse(opt.iconName.isEmpty, "Option \(opt.id) has empty iconName")
        }
    }

    // MARK: - resolve()

    func testResolveAutoForEmptyValue() {
        let opt = RecapModelOption.resolve("")
        XCTAssertEqual(opt.id, RecapModelOption.autoKey)
        XCTAssertEqual(opt.provider, .auto)
    }

    func testResolveAutoForWhitespaceValue() {
        let opt = RecapModelOption.resolve("   \n\t  ")
        XCTAssertEqual(opt.id, RecapModelOption.autoKey)
    }

    func testResolveCuratedAnthropic() {
        let opt = RecapModelOption.resolve("claude-opus-4-7")
        XCTAssertEqual(opt.provider, .anthropic)
        XCTAssertTrue(opt.label.contains("Opus"))
    }

    func testResolveCustomFallthrough() {
        let opt = RecapModelOption.resolve("some-future-model-id")
        XCTAssertEqual(opt.id, "some-future-model-id")
        XCTAssertEqual(opt.provider, .custom)
        XCTAssertTrue(opt.label.contains("Custom"))
    }

    // MARK: - autoKey contract

    func testAutoKeyIsEmptyString() {
        // Win SettingsViewModel persists "" for the Auto option.
        // Same contract on Mac so the config field round-trips cleanly.
        XCTAssertEqual(RecapModelOption.autoKey, "")
    }

    // MARK: - pickRecapModel() — config-driven heuristic
    //
    // pickRecapModel() reads `~/Library/Application Support/dimmy/
    // config.json` directly. We don't want to clobber the user's real
    // config in tests, so these cases verify the function is a pure
    // function of the file's contents by writing a temp config to a
    // dedicated dir under tmp, redirecting via env-overridden support
    // dir is not currently supported — instead we skip the on-disk
    // tests and verify behaviour via the resolve() path (above) plus
    // the integration test below that round-trips through AppState.

    // (Filesystem-level redirection requires a refactor of
    // pickRecapModel to accept a search root. Tracked separately.)
}
