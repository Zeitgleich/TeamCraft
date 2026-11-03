import Foundation

/// Build settings, read from the Info.plist of the running bundle (app or extension); the values
/// come from `Config/Base.xcconfig`. Nothing here is secret, and no server is baked in: the user
/// enters it (docs/operations.md "Server address in the apps"), see `ServerStore`.
struct AppEnvironment: Equatable {
  /// Development builds also accept plain `http` to other devices; release builds only to loopback.
  #if DEBUG
    static let allowsInsecureHttp = true
  #else
    static let allowsInsecureHttp = false
  #endif

  /// The push gateway this build seals its APNs token for (a publisher's store app); `nil` uses the
  /// gateway the server names.
  let pushGatewayURL: URL?
  /// A server address compiled into a self-built app (`TEAMCRAFT_DEFAULT_SERVER_URL`), offered at
  /// the first start like a managed server that isn't locked. As typed there; checked like input.
  let maybeDefaultServerURL: String?
  /// The redirect URI is `<scheme>:/oauth/callback`.
  let oidcRedirectScheme: String
  let appGroupId: String
  /// Empty when the build isn't signed with a team (simulator without signing).
  let keychainAccessGroup: String
  /// `CFBundleShortVersionString`; the server tells whether it still accepts this version.
  let clientVersion: String

  var oidcRedirectURI: URL {
    // Invariant: the scheme comes from the build settings and is a valid URL scheme.
    URL(string: "\(oidcRedirectScheme):/oauth/callback")!
  }

  static let current = AppEnvironment(bundle: .main)

  init(bundle: Bundle) {
    func string(_ key: String) -> String {
      (bundle.object(forInfoDictionaryKey: key) as? String) ?? ""
    }
    pushGatewayURL = URL(string: string("TCPushGatewayURL")).flatMap { $0.scheme == nil ? nil : $0 }
    let defaultServer = string("TCDefaultServerURL").trimmingCharacters(in: .whitespaces)
    maybeDefaultServerURL = defaultServer.isEmpty ? nil : defaultServer
    oidcRedirectScheme = string("TCOidcRedirectScheme")
    appGroupId = string("TCAppGroupId")
    keychainAccessGroup = string("TCKeychainAccessGroup")
    clientVersion = string("CFBundleShortVersionString")
  }

  init(
    pushGatewayURL: URL? = nil, maybeDefaultServerURL: String? = nil, oidcRedirectScheme: String,
    appGroupId: String, keychainAccessGroup: String, clientVersion: String
  ) {
    self.pushGatewayURL = pushGatewayURL
    self.maybeDefaultServerURL = maybeDefaultServerURL
    self.oidcRedirectScheme = oidcRedirectScheme
    self.appGroupId = appGroupId
    self.keychainAccessGroup = keychainAccessGroup
    self.clientVersion = clientVersion
  }
}
