import Observation
import SwiftUI
import UIKit
import XCTest

@testable import TeamCraft

/// The colour theme setting (#1493): the stored value, the colours from the core, the sync's writes.
@MainActor
final class ColorThemeTests: XCTestCase {
  private var defaults: UserDefaults!

  override func setUp() {
    defaults = UserDefaults(suiteName: "color-theme-tests-\(UUID().uuidString)")
  }

  private func resolve(_ color: Color, dark: Bool) -> UInt32 {
    let ui = UIColor(color).resolvedColor(with: UITraitCollection(userInterfaceStyle: dark ? .dark : .light))
    var (r, g, b): (CGFloat, CGFloat, CGFloat) = (0, 0, 0)
    ui.getRed(&r, green: &g, blue: &b, alpha: nil)
    return UInt32((r * 255).rounded()) << 16 | UInt32((g * 255).rounded()) << 8 | UInt32((b * 255).rounded())
  }

  func testAThemeTheBrandDoesNotAllowShowsTheDefaultAndStaysStored() throws {
    defaults.set("preset:paper", forKey: ColorThemeSetting.key)
    let model = ColorThemeModel(defaults: defaults)
    let open = try XCTUnwrap(model.maybeColors?.light.chrome)

    model.usePolicy(
      AppThemePolicy(defaultValue: "preset:harbor", maybeAllowedPresets: ["pine"], isCustomAllowed: false))
    XCTAssertEqual(model.value, "preset:paper")
    XCTAssertEqual(ColorThemeSetting.stored(in: defaults), "preset:paper")
    XCTAssertEqual(model.effectiveValue, "preset:harbor")
    XCTAssertEqual(model.inForce, ColorThemeSetting.defaultValue)
    XCTAssertTrue(model.isWithheld)
    XCTAssertFalse(model.canCustomize)
    XCTAssertTrue(model.hasChoices)
    XCTAssertTrue(model.allowsPreset(id: "pine"))
    XCTAssertFalse(model.allowsPreset(id: "paper"))
    XCTAssertEqual(
      model.maybeColors?.light.chrome, ColorThemeModel.colors(of: "preset:harbor")?.light.chrome)

    model.usePolicy(nil)
    XCTAssertEqual(model.maybeColors?.light.chrome, open)
    XCTAssertFalse(model.isWithheld)
  }

  func testAPolicyWithoutChoicesIsAFixedLook() {
    let model = ColorThemeModel(defaults: defaults)
    model.usePolicy(AppThemePolicy(defaultValue: "brand", maybeAllowedPresets: [], isCustomAllowed: false))
    XCTAssertFalse(model.hasChoices)
    XCTAssertFalse(model.isThemed)
  }

  func testAnEmptyDeviceHasTheDefault() {
    XCTAssertEqual(ColorThemeSetting.stored(in: defaults), "brand")
    let model = ColorThemeModel(defaults: defaults)
    XCTAssertEqual(model.value, "brand")
    XCTAssertFalse(model.isThemed)
  }

  func testTheDefaultKeepsTheAppsOwnColoursInLightAndDark() {
    let model = ColorThemeModel(defaults: defaults)
    let accent = model.color(\.accent, fallback: (0x005DFC, 0x5C9DFF))
    XCTAssertEqual(resolve(accent, dark: false), 0x005DFC)
    XCTAssertEqual(resolve(accent, dark: true), 0x5C9DFF)
  }

  func testAnUnknownValueShowsTheDefaultAndIsNotOverwritten() {
    for unknown in ["preset:from-the-future", "custom:nope", "gibberish", ""] {
      defaults.set(unknown, forKey: ColorThemeSetting.key)
      let model = ColorThemeModel(defaults: defaults)
      XCTAssertFalse(model.isThemed, unknown)
      XCTAssertEqual(model.value, unknown)
      model.reload()
      XCTAssertEqual(defaults.string(forKey: ColorThemeSetting.key), unknown, "reading must not write")
    }
  }

  func testAPresetGivesTheCoresColoursForLightAndDark() throws {
    let model = ColorThemeModel(defaults: defaults)
    let preset = try XCTUnwrap(colorThemePresets().first)
    model.choose(preset.value)
    XCTAssertTrue(model.isThemed)
    XCTAssertEqual(defaults.string(forKey: ColorThemeSetting.key), preset.value)
    let light = try XCTUnwrap(colorThemeTokens(value: preset.value, isDark: false))
    let dark = try XCTUnwrap(colorThemeTokens(value: preset.value, isDark: true))
    let accent = model.color(\.accent, fallback: (0, 0))
    XCTAssertEqual(resolve(accent, dark: false), light.accent)
    XCTAssertEqual(resolve(accent, dark: true), dark.accent)
  }

  func testEveryPresetResolvesAndTheDefaultDoesNot() {
    for preset in colorThemePresets() {
      XCTAssertNotNil(ColorThemeModel.colors(of: preset.value), preset.id)
    }
    XCTAssertNil(ColorThemeModel.colors(of: "brand"))
  }

  func testACustomThemeRoundTripsThroughTheCore() throws {
    let model = ColorThemeModel(defaults: defaults)
    let value = try XCTUnwrap(colorThemeCustom(colors: [0x1B2233, 0x2F66E0, 0x2EB67D, 0xD1453B]))
    model.choose(value)
    XCTAssertEqual(model.value, "custom:#1b2233,#2f66e0,#2eb67d,#d1453b")
    XCTAssertTrue(model.isThemed)
    XCTAssertEqual(colorThemeColors(value: model.value), [0x1B2233, 0x2F66E0, 0x2EB67D, 0xD1453B])
  }

  func testChoosingTheDefaultAgainReturnsToTheAppsColours() throws {
    let model = ColorThemeModel(defaults: defaults)
    model.choose(try XCTUnwrap(colorThemePresets().first).value)
    model.choose("brand")
    XCTAssertFalse(model.isThemed)
    XCTAssertEqual(defaults.string(forKey: ColorThemeSetting.key), "brand")
  }

  func testAChangeIsObservedSoOpenScreensBuildAgain() throws {
    let model = ColorThemeModel(defaults: defaults)
    var changes = 0
    withObservationTracking {
      _ = model.color(\.accent, fallback: (1, 2))
    } onChange: {
      changes += 1
    }
    model.choose(try XCTUnwrap(colorThemePresets().first).value)
    XCTAssertEqual(changes, 1)
  }

  func testAValueTheSyncWritesIsPickedUp() async throws {
    let model = ColorThemeModel(defaults: defaults)
    let stores = AppSettingsStores(
      defaults: defaults, readMentions: { AppSettingsStores.defaultMentions }, writeMentions: { _ in })
    let before = await stores.current()
    var new = before
    new.colorTheme = try XCTUnwrap(colorThemePresets().last).value
    await stores.apply(new, over: before)
    let picked = expectation(description: "the model follows the defaults")
    Task { @MainActor in
      while model.value != new.colorTheme { try? await Task.sleep(for: .milliseconds(10)) }
      picked.fulfill()
    }
    await fulfillment(of: [picked], timeout: 3)
    XCTAssertTrue(model.isThemed)
    let current = await stores.current()
    XCTAssertEqual(current.colorTheme, new.colorTheme)
  }

  func testTheSyncKeepsAnUnknownValueItDidNotChange() async {
    defaults.set("preset:from-the-future", forKey: ColorThemeSetting.key)
    let stores = AppSettingsStores(
      defaults: defaults, readMentions: { AppSettingsStores.defaultMentions }, writeMentions: { _ in })
    let before = await stores.current()
    XCTAssertEqual(before.colorTheme, "preset:from-the-future")
    var new = before
    new.sharesTyping = false
    await stores.apply(new, over: before)
    XCTAssertEqual(defaults.string(forKey: ColorThemeSetting.key), "preset:from-the-future")
  }

  func testHexFieldsTakeShortAndLongFormsWithOrWithoutTheHash() {
    XCTAssertEqual(ColorThemeSetting.parseHex("#1b2233"), 0x1B2233)
    XCTAssertEqual(ColorThemeSetting.parseHex("1B2233"), 0x1B2233)
    XCTAssertEqual(ColorThemeSetting.parseHex(" #abc "), 0xAABBCC)
    XCTAssertEqual(ColorThemeSetting.parseHex("fff"), 0xFFFFFF)
    for bad in ["", "#", "#12", "#1234", "#12345", "#1234567", "#ggg", "#12 456", "red", "##123456"] {
      XCTAssertNil(ColorThemeSetting.parseHex(bad), bad)
    }
    XCTAssertEqual(ColorThemeSetting.hex(0x00ABCD), "#00abcd")
  }

  func testPresetNamesCoverEveryPresetOfTheCore() {
    // A preset without a name would show its id; the picker must know all of them.
    let known: Set<String> = [
      "harbor", "graphite", "pine", "deep-sea", "heather", "paper", "dune", "meadow", "lavender", "ember",
      "orchid", "lagoon", "indigo", "red-green-safe", "blue-yellow-safe", "high-contrast",
    ]
    XCTAssertEqual(Set(colorThemePresets().map(\.id)), known)
  }
}
