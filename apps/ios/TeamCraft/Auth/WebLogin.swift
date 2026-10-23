import AuthenticationServices
import Foundation
import UIKit

/// The browser half of the OIDC login: authorization code with PKCE in an
/// `ASWebAuthenticationSession` (the system browser's cookies give single sign-on), then the code
/// exchange through `OidcClient`, with the sign-in settings the server announced.
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
    let url = Self.authorizationURL(endpoints: endpoints, client: oidc, pkce: pkce, state: state)
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
      maybeSession = session
      if !session.start() {
        continuation.resume(throwing: OidcError.cancelled)
      }
    }
  }

  static func authorizationURL(
    endpoints: OidcClient.Endpoints, client: OidcClient, pkce: Pkce, state: String
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
