import Foundation

/// Deployment settings, read from the Info.plist of the running bundle (app or extension); the values
/// come from `Config/Base.xcconfig`. Nothing here is secret.
struct AppEnvironment: Equatable {
  /// The TeamCraft server, e.g. `https://chat.example.com`.
  let serverURL: URL
  /// OIDC issuer (Keycloak realm URL).
  let oidcIssuer: URL
  /// Public OIDC client (authorization code + PKCE).
  let oidcClientId: String
  /// The redirect URI is `<scheme>:/oauth/callback`.
  let oidcRedirectScheme: String
  let appGroupId: String
  /// Empty when the build isn't signed with a team (simulator without signing).
  let keychainAccessGroup: String

  var oidcRedirectURI: URL {
    URL(string: "\(oidcRedirectScheme):/oauth/callback")!
  }

  static let current = AppEnvironment(bundle: .main)

  init(bundle: Bundle) {
    func string(_ key: String) -> String {
      (bundle.object(forInfoDictionaryKey: key) as? String) ?? ""
    }
    serverURL = URL(string: string("TCServerURL")) ?? URL(string: "http://127.0.0.1:8081")!
    oidcIssuer = URL(string: string("TCOidcIssuer")) ?? URL(string: "http://127.0.0.1:8080")!
    oidcClientId = string("TCOidcClientId")
    oidcRedirectScheme = string("TCOidcRedirectScheme")
    appGroupId = string("TCAppGroupId")
    keychainAccessGroup = string("TCKeychainAccessGroup")
  }

  init(
    serverURL: URL, oidcIssuer: URL, oidcClientId: String, oidcRedirectScheme: String,
    appGroupId: String, keychainAccessGroup: String
  ) {
    self.serverURL = serverURL
    self.oidcIssuer = oidcIssuer
    self.oidcClientId = oidcClientId
    self.oidcRedirectScheme = oidcRedirectScheme
    self.appGroupId = appGroupId
    self.keychainAccessGroup = keychainAccessGroup
  }
}
