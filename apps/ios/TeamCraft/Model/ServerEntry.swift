import Foundation
import SwiftUI

/// What the app does at start about the server (docs/operations.md "Server address in the apps").
/// Pure, so the precedence of stored account, managed configuration and build default is tested.
enum ServerStartDecision: Equatable {
  /// Go on with the stored account's server.
  case useStored
  /// The device's management locks another server: sign the stored account out, forget it, then
  /// check `address` and ask for confirmation.
  case forgetStoredAndCheck(address: String)
  /// No account yet, but a server is preconfigured: check it and ask for confirmation.
  case check(address: String)
  /// Ask the user for the server address.
  case enterServer

  /// `normalize` turns an address into its origin (`nil`: unusable), as `normalizeServerAddress`.
  static func decide(
    maybeStoredOrigin: String?, managed: ManagedServer, normalize: (String) -> String?
  ) -> ServerStartDecision {
    if let storedOrigin = maybeStoredOrigin {
      if managed.allows(origin: storedOrigin, normalize: normalize) { return .useStored }
      // Invariant: `allows` only refuses when a locked managed address exists.
      return .forgetStoredAndCheck(address: managed.maybeAddress ?? "")
    }
    if let address = managed.maybeAddress { return .check(address: address) }
    return .enterServer
  }
}

/// A server named by the device rather than by the user: the managed app configuration of an MDM
/// (`server_url`, `lock_server`), else the default compiled into a self-built app, which is never
/// locked.
struct ManagedServer: Equatable {
  /// Where iOS puts an MDM's managed app configuration.
  static let configurationKey = "com.apple.configuration.managed"
  static let serverURLKey = "server_url"
  static let lockServerKey = "lock_server"

  /// As configured; checked like user input.
  var maybeAddress: String?
  /// The user may not use another server.
  var isLocked: Bool

  /// The configuration of this device and build.
  static func current(
    defaults: UserDefaults = .standard, environment: AppEnvironment = .current
  ) -> ManagedServer {
    resolve(managed: defaults.dictionary(forKey: configurationKey), buildDefault: environment.maybeDefaultServerURL)
  }

  /// Managed configuration first; a lock without a server address locks nothing.
  static func resolve(managed maybeManaged: [String: Any]?, buildDefault maybeBuildDefault: String?) -> ManagedServer {
    func nonEmpty(_ value: String?) -> String? {
      guard let trimmed = value?.trimmingCharacters(in: .whitespacesAndNewlines), !trimmed.isEmpty else { return nil }
      return trimmed
    }
    if let address = nonEmpty(maybeManaged?[serverURLKey] as? String) {
      return ManagedServer(maybeAddress: address, isLocked: (maybeManaged?[lockServerKey] as? Bool) ?? false)
    }
    return ManagedServer(maybeAddress: nonEmpty(maybeBuildDefault), isLocked: false)
  }

  /// Whether the server at `origin` may be used: always unless locked, then only the managed one.
  func allows(origin: String, normalize: (String) -> String?) -> Bool {
    guard isLocked else { return true }
    return maybeAddress.flatMap(normalize) == origin
  }
}

/// Why a server address can't be used, as the server entry shows it (translated per case).
enum ServerFailure: Equatable {
  case invalidAddress, httpsOnly, notTeamCraft, unreachable, tls, captivePortal, unavailable
  case appTooOld, serverTooOld, invalidSignIn, notSetUpForApp, locked

  /// A failed request for the server's answer.
  init(transport error: URLError) {
    switch error.code {
    case .secureConnectionFailed, .serverCertificateHasBadDate, .serverCertificateUntrusted,
      .serverCertificateHasUnknownRoot, .serverCertificateNotYetValid, .clientCertificateRejected,
      .clientCertificateRequired:
      self = .tls
    case .appTransportSecurityRequiresSecureConnection:
      self = .httpsOnly
    case .unsupportedURL, .badURL:
      self = .invalidAddress
    case .dataLengthExceedsMaximum, .badServerResponse, .cannotDecodeRawData, .cannotParseResponse:
      self = .notTeamCraft
    default:
      // Offline, host not found, refused, timed out, connection lost and the like.
      self = .unreachable
    }
  }

  /// A refusal of the core's checks (`ServerEntryError`). The generated enum's case spelling
  /// depends on the UniFFI version, so this reads the description (`NotTeamCraft(message: ...)`),
  /// like `CoreFailure`.
  init(entryError error: Error) {
    self.init(entryErrorDescription: String(describing: error))
  }

  init(entryErrorDescription description: String) {
    let name = description.lowercased()
    let cases: [(String, ServerFailure)] = [
      ("empty", .invalidAddress), ("invalidaddress", .invalidAddress), ("hascredentials", .invalidAddress),
      ("httpsonly", .httpsOnly), ("notteamcraft", .notTeamCraft), ("captiveportal", .captivePortal),
      ("unavailable", .unavailable), ("apptooold", .appTooOld), ("servertooold", .serverTooOld),
      ("invalidsignin", .invalidSignIn), ("redirectnotallowed", .notSetUpForApp),
    ]
    self = cases.first { name.hasPrefix($0.0) }?.1 ?? .notTeamCraft
  }

  var message: LocalizedStringKey {
    switch self {
    case .invalidAddress: return "server_error_invalid_address"
    case .httpsOnly: return "server_error_https_only"
    case .notTeamCraft: return "server_error_not_teamcraft"
    case .unreachable: return "server_error_unreachable"
    case .tls: return "server_error_tls"
    case .captivePortal: return "server_error_captive_portal"
    case .unavailable: return "server_error_unavailable"
    case .appTooOld: return "server_error_app_too_old"
    case .serverTooOld: return "server_error_server_too_old"
    case .invalidSignIn: return "server_error_invalid_sign_in"
    case .notSetUpForApp: return "server_error_not_set_up"
    case .locked: return "server_error_locked"
    }
  }
}

/// Fetches `GET /api/v1/server-info` the way the core's check expects it: no redirects followed (a
/// redirect means a captive portal or the wrong server), no cookies or cache, a bounded body.
struct ServerInfoClient {
  static let maxBodyBytes = 1024 * 1024

  private static let session: URLSession = {
    let configuration = URLSessionConfiguration.ephemeral
    configuration.timeoutIntervalForRequest = 15
    configuration.timeoutIntervalForResource = 30
    configuration.requestCachePolicy = .reloadIgnoringLocalCacheData
    configuration.httpShouldSetCookies = false
    return URLSession(configuration: configuration)
  }()

  /// The answer as received; throws `URLError` when there is none.
  func fetch(_ url: URL) async throws -> ServerInfoAnswer {
    var request = URLRequest(url: url)
    request.setValue("application/json", forHTTPHeaderField: "Accept")
    let (bytes, response) = try await Self.session.bytes(for: request, delegate: RedirectRefuser())
    guard let http = response as? HTTPURLResponse else { throw URLError(.badServerResponse) }
    if http.expectedContentLength > Int64(Self.maxBodyBytes) { throw URLError(.dataLengthExceedsMaximum) }
    var body = Data()
    for try await byte in bytes {
      body.append(byte)
      if body.count > Self.maxBodyBytes { throw URLError(.dataLengthExceedsMaximum) }
    }
    return ServerInfoAnswer(
      status: UInt16(clamping: http.statusCode), contentType: http.value(forHTTPHeaderField: "Content-Type"),
      body: body)
  }
}

/// An HTTP answer for `checkServerInfo`.
struct ServerInfoAnswer: Equatable {
  let status: UInt16
  let contentType: String?
  let body: Data
}

/// Hands the redirect response itself back instead of following it.
private final class RedirectRefuser: NSObject, URLSessionTaskDelegate {
  func urlSession(
    _ session: URLSession, task: URLSessionTask, willPerformHTTPRedirection response: HTTPURLResponse,
    newRequest request: URLRequest, completionHandler: @escaping (URLRequest?) -> Void
  ) {
    completionHandler(nil)
  }
}
