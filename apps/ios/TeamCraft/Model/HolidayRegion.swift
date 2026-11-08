import Foundation

/// Whose public holidays the member's repeating meetings skip (`/me/holiday-region`, #1450): the
/// workspace's region, or one of their own. `region` is absent while the workspace's applies.
struct HolidayRegionState: Decodable, Equatable, Sendable {
  let region: String?
  /// The workspace's region, which applies without an own one; empty: none.
  let instanceRegion: String
  /// The workspace has a holiday calendar whose holidays are skipped as well.
  let hasInstanceFeed: Bool

  init(region: String?, instanceRegion: String, hasInstanceFeed: Bool) {
    self.region = region
    self.instanceRegion = instanceRegion
    self.hasInstanceFeed = hasInstanceFeed
  }

  /// Fields an older or newer server leaves out or adds don't matter.
  init(from decoder: Decoder) throws {
    let container = try decoder.container(keyedBy: Keys.self)
    region = try container.decodeIfPresent(String.self, forKey: .region)
    instanceRegion = try container.decodeIfPresent(String.self, forKey: .instanceRegion) ?? ""
    hasInstanceFeed = try container.decodeIfPresent(Bool.self, forKey: .hasInstanceFeed) ?? false
  }

  private enum Keys: String, CodingKey {
    case region, instanceRegion, hasInstanceFeed
  }
}

/// The regions of the server's holiday table (`HOLIDAY_REGIONS`), in its order, and their names.
enum HolidayRegions {
  static let codes: [String] = [
    "DE", "DE-BW", "DE-BY", "DE-BE", "DE-BB", "DE-HB", "DE-HH", "DE-HE", "DE-MV", "DE-NI", "DE-NW", "DE-RP",
    "DE-SL", "DE-SN", "DE-ST", "DE-SH", "DE-TH", "AT", "CH", "CH-AG", "CH-AI", "CH-AR", "CH-BE", "CH-BL",
    "CH-BS", "CH-FR", "CH-GE", "CH-GL", "CH-GR", "CH-JU", "CH-LU", "CH-NE", "CH-NW", "CH-OW", "CH-SG", "CH-SH",
    "CH-SO", "CH-SZ", "CH-TG", "CH-TI", "CH-UR", "CH-VD", "CH-VS", "CH-ZG", "CH-ZH",
  ]

  /// The catalog key of a region's name; an empty code is "None" (a workspace without a region).
  static func titleKey(for code: String) -> String {
    "holiday_region_" + (code.isEmpty ? "none" : code.lowercased().replacingOccurrences(of: "-", with: "_"))
  }

  /// A region's name in the user's language, for texts that carry it.
  static func name(for code: String) -> String {
    let key = titleKey(for: code)
    let name = Bundle.main.localizedString(forKey: key, value: nil, table: nil)
    // A region of a newer server that has no name here shows as its code.
    return name == key ? code : name
  }

  /// The body of `PUT /me/holiday-region`: no region goes back to the workspace's.
  static func requestBody(region maybeRegion: String?) -> String {
    guard let region = maybeRegion, !region.isEmpty else { return "{}" }
    return jsonText(["region": region])
  }
}

/// How saving the region ended: what the server stored, or what to say at the picker.
enum HolidayRegionSave {
  /// `nil`: saved, but the answer could not be read; the region is read again.
  case saved(HolidayRegionState?)
  case failed(TextProblem)
}

extension AppModel {
  /// The member's region, or `nil` offline or when the server refuses (guests).
  func holidayRegion() async -> HolidayRegionState? {
    guard let json = try? await core().apiRequest(method: "GET", path: "/me/holiday-region", maybeBody: nil)
    else { return nil }
    return Wire.decode(HolidayRegionState.self, from: json)
  }

  /// Picks `maybeRegion` (`nil`: the workspace's). The meeting list reads the meetings afresh
  /// whenever it is opened, so it shows the occurrences that are skipped now.
  func setHolidayRegion(_ maybeRegion: String?) async -> HolidayRegionSave {
    let body = HolidayRegions.requestBody(region: maybeRegion)
    do {
      let json = try await core().apiRequest(method: "PUT", path: "/me/holiday-region", maybeBody: body)
      return .saved(Wire.decode(HolidayRegionState.self, from: json))
    } catch {
      switch CoreFailure(error) {
      case .other: return .failed(.rejected("holiday_save_failed"))
      case let failure: return .failed(.refused(AppFailure(failure)))
      }
    }
  }
}
