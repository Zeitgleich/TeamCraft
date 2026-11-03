import Foundation
import UIKit

/// The server's brand for the server confirmation and sign-in screens (docs/branding.md §5): asked
/// without credentials from the selected server, checked by the core (`loginBrand`), cached per
/// server origin so the next start shows it at once. Brand data only changes what the screen
/// shows, never the server.
@MainActor
@Observable
final class BrandStore {
  private static let maxBrandJSONBytes = 64 * 1024
  private static let maxLogoBytes = 512 * 1024

  private(set) var brand: LoginBrand?
  private(set) var logo: UIImage?
  private(set) var darkLogo: UIImage?
  /// The brand's square icon, for its tile beside TeamCraft's on the sign-in screen (issue #463).
  private(set) var icon: UIImage?
  private(set) var darkIcon: UIImage?
  /// The server the brand belongs to; `nil` before a server is chosen.
  private(set) var maybeAddress: ServerAddress?

  private let session: URLSession

  init() {
    let configuration = URLSessionConfiguration.ephemeral
    configuration.timeoutIntervalForRequest = 10
    session = URLSession(configuration: configuration)
  }

  /// The server's name as the screens show it.
  var serverHost: String { maybeAddress?.host ?? "" }

  /// Shows the brand of `address`: the cached one, or `initial` (from the server check) when given.
  func select(_ address: ServerAddress?, initial maybeInitial: LoginBrand? = nil) {
    maybeAddress = address
    brand = nil
    logo = nil
    darkLogo = nil
    icon = nil
    darkIcon = nil
    guard let address else { return }
    if let json = UserDefaults.standard.string(forKey: Self.cacheKey(address)) {
      brand = check(json, address: address)
      logo = cachedImage(address, name: "logo")
      darkLogo = cachedImage(address, name: "logo_dark")
      icon = cachedImage(address, name: "icon")
      darkIcon = cachedImage(address, name: "icon_dark")
    }
    if let maybeInitial { brand = maybeInitial }
  }

  /// Asks the selected server again; the cached brand stays when it can't be reached.
  func refresh() async {
    guard let address = maybeAddress, let origin = URL(string: address.origin) else { return }
    guard let data = await download(origin.appending(path: "api/v1/brand"), maxBytes: Self.maxBrandJSONBytes),
      let json = String(data: data, encoding: .utf8), isSelected(address)
    else { return }
    guard let checked = check(json, address: address) else {
      brand = nil
      return
    }
    UserDefaults.standard.set(json, forKey: Self.cacheKey(address))
    brand = checked
    let light = await saveImage(checked.maybeLogoUrl, address: address, name: "logo")
    let dark = await saveImage(checked.maybeLogoDarkUrl, address: address, name: "logo_dark")
    let lightIcon = await saveImage(checked.maybeIconUrl, address: address, name: "icon")
    let darkIconImage = await saveImage(checked.maybeIconDarkUrl, address: address, name: "icon_dark")
    // Another server may have been selected while the images loaded.
    guard isSelected(address) else { return }
    logo = light
    darkLogo = dark
    icon = lightIcon
    darkIcon = darkIconImage
  }

  private static func cacheKey(_ address: ServerAddress) -> String {
    "teamcraft.brand:" + address.origin
  }

  private func isSelected(_ address: ServerAddress) -> Bool {
    maybeAddress?.origin == address.origin
  }

  private func check(_ json: String, address: ServerAddress) -> LoginBrand? {
    let language = Locale.preferredLanguages.first ?? "en"
    return loginBrand(responseJson: json, serverUrl: address.origin, language: language)
  }

  private func saveImage(_ maybeURL: String?, address: ServerAddress, name: String) async -> UIImage? {
    let file = imageFile(address, name: name)
    guard let maybeURL, let url = URL(string: maybeURL) else {
      try? FileManager.default.removeItem(at: file)
      return nil
    }
    guard let data = await download(url, maxBytes: Self.maxLogoBytes), let image = UIImage(data: data) else {
      // The previous image (or none) stays; the name is shown either way.
      return cachedImage(address, name: name)
    }
    try? FileManager.default.createDirectory(at: file.deletingLastPathComponent(), withIntermediateDirectories: true)
    try? data.write(to: file, options: .atomic)
    return image
  }

  private func cachedImage(_ address: ServerAddress, name: String) -> UIImage? {
    UIImage(contentsOfFile: imageFile(address, name: name).path(percentEncoded: false))
  }

  private func imageFile(_ address: ServerAddress, name: String) -> URL {
    URL.applicationSupportDirectory.appending(path: "brand").appending(path: address.storageKey)
      .appending(path: name)
  }

  private func download(_ url: URL, maxBytes: Int) async -> Data? {
    // Unreachable or refused: the cached brand (or the app's own name) stays.
    guard let (data, response) = try? await session.data(from: url),
      (response as? HTTPURLResponse)?.statusCode == 200, data.count <= maxBytes
    else { return nil }
    return data
  }
}
