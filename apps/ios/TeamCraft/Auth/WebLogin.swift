import AuthenticationServices
import Foundation
import UIKit

/// The browser half of the OIDC login: authorization code with PKCE in an ephemeral
/// `ASWebAuthenticationSession`, then the code exchange through `OidcClient`, with the sign-in
/// settings the server announced. Ephemeral, and `prompt=login` on top (issue #448): the provider
/// shows its login page instead of silently reusing a browser session, so the user sees which
/// account they sign in with and can switch; the last account is prefilled (`login_hint`).
@MainActor
final class WebLogin: NSObject, ASWebAuthenticationPresentationContextProviding {
  private let environment: AppEnvironment
  private var maybeSession: ASWebAuthenticationSession?

  init(environment: AppEnvironment = .current) {
    self.environment = environment
  }

  func login(server: StoredServer) async throws -> OidcSession {
    guard let oidc = OidcClient(server: server, environment: environment) else { throw OidcError.discoveryFailed }
    let endpoints = try await oidc.discover()
    let pkce = Pkce.generate()
    let state = Pkce.randomState()
    let parameters = LoginParameters(
      server: server, locale: Bundle.main.preferredLocalizations.first ?? Locale.current.identifier)
    let url = Self.authorizationURL(
      endpoints: endpoints, client: oidc, pkce: pkce, state: state, login: parameters)
    let callback = try await present(url)
    let code = try Self.authorizationCode(from: callback, expectedState: state)
    return try await oidc.exchange(code: code, verifier: pkce.verifier, endpoints: endpoints)
  }

  nonisolated func presentationAnchor(for session: ASWebAuthenticationSession) -> ASPresentationAnchor {
    MainActor.assumeIsolated {
      let scene = UIApplication.shared.connectedScenes.compactMap { $0 as? UIWindowScene }.first
      return scene?.keyWindow ?? ASPresentationAnchor()
    }
  }

  private func present(_ url: URL) async throws -> URL {
    try await withCheckedThrowingContinuation { continuation in
      let session = ASWebAuthenticationSession(
        url: url, callbackURLScheme: environment.oidcRedirectScheme
      ) { callback, error in
        if let callback {
          continuation.resume(returning: callback)
        } else if let error = error as? ASWebAuthenticationSessionError, error.code == .canceledLogin {
          continuation.resume(throwing: OidcError.cancelled)
        } else {
          continuation.resume(throwing: OidcError.network(error?.localizedDescription ?? "no answer"))
        }
      }
      session.presentationContextProvider = self
      // No cookies of earlier sign-ins: the provider asks which account (issue #448).
      session.prefersEphemeralWebBrowserSession = true
      maybeSession = session
      if !session.start() {
        continuation.resume(throwing: OidcError.cancelled)
      }
    }
  }

  static func authorizationURL(
    endpoints: OidcClient.Endpoints, client: OidcClient, pkce: Pkce, state: String,
    login: LoginParameters = .none
  ) -> URL {
    var components = URLComponents(url: endpoints.authorizationEndpoint, resolvingAgainstBaseURL: false)!
    var items = components.queryItems ?? []
    items += [
      URLQueryItem(name: "response_type", value: "code"),
      URLQueryItem(name: "client_id", value: client.clientId),
      URLQueryItem(name: "redirect_uri", value: client.redirectURI.absoluteString),
      URLQueryItem(name: "scope", value: client.scopes.joined(separator: " ")),
      URLQueryItem(name: "code_challenge", value: pkce.challenge),
      URLQueryItem(name: "code_challenge_method", value: "S256"),
      URLQueryItem(name: "state", value: state),
    ]
    items += login.queryItems
    components.queryItems = items
    return components.url!
  }

  static func authorizationCode(from callback: URL, expectedState: String) throws -> String {
    let items = URLComponents(url: callback, resolvingAgainstBaseURL: false)?.queryItems ?? []
    func value(_ name: String) -> String? { items.first { $0.name == name }?.value }
    guard value("state") == expectedState else { throw OidcError.stateMismatch }
    guard let code = value("code"), !code.isEmpty else { throw OidcError.invalidResponse }
    return code
  }
}

/// What a sign-in the user starts adds to the authorization request (issue #448): the login page
/// also with an existing provider session (the server's setting), the last account on this device
/// for this server, and the app's language. All OIDC Core parameters.
struct LoginParameters: Equatable {
  var prompt: String?
  var loginHint: String?
  var uiLocales: String?

  static let none = LoginParameters()

  init(prompt: String? = nil, loginHint: String? = nil, uiLocales: String? = nil) {
    self.prompt = prompt
    self.loginHint = loginHint
    self.uiLocales = uiLocales
  }

  init(server: StoredServer, locale: String) {
    let hint = server.maybeLoginHint?.trimmingCharacters(in: .whitespacesAndNewlines)
    let tag = locale.replacingOccurrences(of: "_", with: "-")
    self.init(
      prompt: server.isLoginPageAlwaysShown ? "login" : nil,
      loginHint: hint?.isEmpty == false ? hint : nil,
      uiLocales: tag.isEmpty ? nil : tag)
  }

  var queryItems: [URLQueryItem] {
    [("prompt", prompt), ("login_hint", loginHint), ("ui_locales", uiLocales)].compactMap { name, value in
      value.map { URLQueryItem(name: name, value: $0) }
    }
  }
}
