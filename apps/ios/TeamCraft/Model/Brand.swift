import Foundation
import UIKit

/// The server's brand for the sign-in screen (docs/branding.md §5): asked without credentials from
/// the configured server, checked by the core (`loginBrand`), cached per server URL so the next
/// start shows it at once. Brand data only changes what the screen shows, never the server.
@MainActor
@Observable
final class BrandStore {
  private static let maxBrandJSONBytes = 64 * 1024
  private static let maxLogoBytes = 512 * 1024

  private(set) var brand: LoginBrand?
  private(set) var logo: UIImage?
  private(set) var darkLogo: UIImage?

  private let serverURL: URL
  private let session: URLSession

  init(serverURL: URL) {
    self.serverURL = serverURL
    let configuration = URLSessionConfiguration.ephemeral
    configuration.timeoutIntervalForRequest = 10
    session = URLSession(configuration: configuration)
    if let json = UserDefaults.standard.string(forKey: cacheKey) {
      brand = check(json)
      logo = cachedImage(isDark: false)
      darkLogo = cachedImage(isDark: true)
    }
  }

  /// The server's name as the sign-in screen shows it.
  var serverHost: String { serverURL.host() ?? serverURL.absoluteString }

  /// Asks the server again; the cached brand stays when it can't be reached.
  func refresh() async {
    let url = serverURL.appending(path: "api/v1/brand")
    guard let data = await download(url, maxBytes: Self.maxBrandJSONBytes),
      let json = String(data: data, encoding: .utf8)
    else { return }
    guard let checked = check(json) else {
      brand = nil
      return
    }
    UserDefaults.standard.set(json, forKey: cacheKey)
    brand = checked
    logo = await saveLogo(checked.maybeLogoUrl, isDark: false)
    darkLogo = await saveLogo(checked.maybeLogoDarkUrl, isDark: true)
  }

  private var cacheKey: String {
    "teamcraft.brand:" + serverURL.absoluteString.trimmingCharacters(in: CharacterSet(charactersIn: "/"))
  }

  private func check(_ json: String) -> LoginBrand? {
    let language = Locale.preferredLanguages.first ?? "en"
    return loginBrand(responseJson: json, serverUrl: serverURL.absoluteString, language: language)
  }

  private func saveLogo(_ maybeURL: String?, isDark: Bool) async -> UIImage? {
    let file = logoFile(isDark: isDark)
    guard let maybeURL, let url = URL(string: maybeURL) else {
      try? FileManager.default.removeItem(at: file)
      return nil
    }
    guard let data = await download(url, maxBytes: Self.maxLogoBytes), let image = UIImage(data: data) else {
      // The previous logo (or none) stays; the name is shown either way.
      return cachedImage(isDark: isDark)
    }
    try? FileManager.default.createDirectory(at: file.deletingLastPathComponent(), withIntermediateDirectories: true)
    try? data.write(to: file, options: .atomic)
    return image
  }

  private func cachedImage(isDark: Bool) -> UIImage? {
    UIImage(contentsOfFile: logoFile(isDark: isDark).path(percentEncoded: false))
  }

  private func logoFile(isDark: Bool) -> URL {
    URL.applicationSupportDirectory.appending(path: "brand").appending(path: isDark ? "logo_dark" : "logo")
  }

  private func download(_ url: URL, maxBytes: Int) async -> Data? {
    // Unreachable or refused: the cached brand (or the app's own name) stays.
    guard let (data, response) = try? await session.data(from: url),
      (response as? HTTPURLResponse)?.statusCode == 200, data.count <= maxBytes
    else { return nil }
    return data
  }
}
