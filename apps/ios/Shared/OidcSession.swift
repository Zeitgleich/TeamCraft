import Foundation

/// The signed-in session: tokens of the OIDC login. Kept in the shared keychain (never in
/// UserDefaults) so the extension can fetch a message with it while the app is closed.
struct OidcSession: Codable, Equatable {
  var accessToken: String
  var refreshToken: String?
  /// When the access token stops being valid.
  var expiresAt: Date
  /// The OIDC subject: the account key for local data.
  var subject: String
  var displayName: String?
  /// Endpoints, so a refresh needs no discovery round trip.
  var tokenEndpoint: URL

  /// Valid for at least `leeway` more seconds.
  func isFresh(leeway: TimeInterval = 30, now: Date = Date()) -> Bool {
    expiresAt.timeIntervalSince(now) > leeway
  }
}

enum OidcError: Error, Equatable {
  case discoveryFailed
  case cancelled
  case invalidResponse
  case stateMismatch
  case tokenRequestFailed(status: Int)
  /// The refresh token was rejected: sign in again.
  case sessionExpired
  case network(String)
}

/// What an OIDC provider answers a token request with.
struct TokenResponse: Decodable, Equatable {
  let accessToken: String
  let refreshToken: String?
  let idToken: String?
  let expiresIn: TimeInterval?

  enum CodingKeys: String, CodingKey {
    case accessToken = "access_token"
    case refreshToken = "refresh_token"
    case idToken = "id_token"
    case expiresIn = "expires_in"
  }
}

/// The claims of an ID token that the app uses. The token comes straight from the token endpoint
/// over TLS, so its signature isn't checked here; the server verifies access tokens itself.
struct IdTokenClaims: Equatable {
  let subject: String
  let name: String?

  init?(jwt: String) {
    let parts = jwt.split(separator: ".")
    guard parts.count >= 2,
      let payload = PushPayload.decodeBase64URL(String(parts[1])),
      let json = try? JSONSerialization.jsonObject(with: payload) as? [String: Any],
      let subject = json["sub"] as? String
    else { return nil }
    self.subject = subject
    self.name = (json["name"] as? String) ?? (json["preferred_username"] as? String)
  }
}

/// Discovery, code exchange and refresh against the issuer (RFC 6749, OIDC Core, PKCE RFC 7636).
/// The browser part of the login lives in the app target (`WebLogin`).
struct OidcClient {
  let environment: AppEnvironment
  var urlSession: URLSession = .shared

  struct Endpoints: Decodable, Equatable {
    let authorizationEndpoint: URL
    let tokenEndpoint: URL

    enum CodingKeys: String, CodingKey {
      case authorizationEndpoint = "authorization_endpoint"
      case tokenEndpoint = "token_endpoint"
    }
  }

  func discover() async throws -> Endpoints {
    let url = environment.oidcIssuer.appendingPathComponent(".well-known/openid-configuration")
    do {
      let (data, response) = try await urlSession.data(from: url)
      guard (response as? HTTPURLResponse)?.statusCode == 200 else { throw OidcError.discoveryFailed }
      return try JSONDecoder().decode(Endpoints.self, from: data)
    } catch let error as OidcError {
      throw error
    } catch {
      throw OidcError.network(error.localizedDescription)
    }
  }

  func exchange(code: String, verifier: String, endpoints: Endpoints) async throws -> OidcSession {
    let response = try await tokenRequest(
      endpoints.tokenEndpoint,
      [
        "grant_type": "authorization_code",
        "code": code,
        "redirect_uri": environment.oidcRedirectURI.absoluteString,
        "client_id": environment.oidcClientId,
        "code_verifier": verifier,
      ])
    guard let idToken = response.idToken, let claims = IdTokenClaims(jwt: idToken) else {
      throw OidcError.invalidResponse
    }
    return OidcSession(
      accessToken: response.accessToken, refreshToken: response.refreshToken,
      expiresAt: Date().addingTimeInterval(response.expiresIn ?? 300), subject: claims.subject,
      displayName: claims.name, tokenEndpoint: endpoints.tokenEndpoint)
  }

  func refresh(_ session: OidcSession) async throws -> OidcSession {
    guard let refreshToken = session.refreshToken else { throw OidcError.sessionExpired }
    let response = try await tokenRequest(
      session.tokenEndpoint,
      [
        "grant_type": "refresh_token",
        "refresh_token": refreshToken,
        "client_id": environment.oidcClientId,
      ])
    var renewed = session
    renewed.accessToken = response.accessToken
    // The provider may rotate the refresh token; keep the old one if it doesn't.
    renewed.refreshToken = response.refreshToken ?? refreshToken
    renewed.expiresAt = Date().addingTimeInterval(response.expiresIn ?? 300)
    return renewed
  }

  private func tokenRequest(_ endpoint: URL, _ form: [String: String]) async throws -> TokenResponse {
    var request = URLRequest(url: endpoint)
    request.httpMethod = "POST"
    request.timeoutInterval = 15
    request.setValue("application/x-www-form-urlencoded", forHTTPHeaderField: "Content-Type")
    request.httpBody = Self.formEncoded(form).data(using: .utf8)
    do {
      let (data, response) = try await urlSession.data(for: request)
      let status = (response as? HTTPURLResponse)?.statusCode ?? 0
      switch status {
      case 200: return try JSONDecoder().decode(TokenResponse.self, from: data)
      case 400, 401: throw OidcError.sessionExpired
      default: throw OidcError.tokenRequestFailed(status: status)
      }
    } catch let error as OidcError {
      throw error
    } catch {
      throw OidcError.network(error.localizedDescription)
    }
  }

  static func formEncoded(_ fields: [String: String]) -> String {
    let allowed = CharacterSet.alphanumerics.union(CharacterSet(charactersIn: "-._~"))
    return fields.keys.sorted().map { key in
      let value = fields[key] ?? ""
      return "\(key.addingPercentEncoding(withAllowedCharacters: allowed) ?? key)="
        + "\(value.addingPercentEncoding(withAllowedCharacters: allowed) ?? value)"
    }.joined(separator: "&")
  }
}

/// Keeps the session in the shared keychain and hands the core access tokens, refreshing when
/// needed. Used by both processes; refreshes run under the cross-process lock's protection by the
/// callers (the engine only asks while its process holds the lock).
final class SessionVault {
  private static let account = "oidc-session"

  private let keychain: KeychainStore
  private let oidc: OidcClient
  private let queue = DispatchQueue(label: "com.example.teamcraft.session")

  init(keychain: KeychainStore = KeychainStore(), oidc: OidcClient = OidcClient(environment: .current)) {
    self.keychain = keychain
    self.oidc = oidc
  }

  func load() -> OidcSession? {
    guard let data = try? keychain.read(Self.account) else { return nil }
    return try? JSONDecoder().decode(OidcSession.self, from: data)
  }

  func save(_ session: OidcSession) throws {
    try keychain.write(JSONEncoder().encode(session), for: Self.account)
  }

  func clear() {
    try? keychain.delete(Self.account)
  }

  /// A valid access token, refreshed if it is about to expire; `nil` when signed out or the
  /// refresh failed. Blocks (up to the request timeout): the core asks on its engine thread.
  func currentAccessToken() -> String? {
    queue.sync {
      guard let session = load() else { return nil }
      if session.isFresh() { return session.accessToken }
      guard session.refreshToken != nil else { return nil }
      let semaphore = DispatchSemaphore(value: 0)
      let box = ResultBox()
      let oidc = self.oidc
      Task.detached {
        do { box.outcome = .success(try await oidc.refresh(session)) } catch { box.outcome = .failure(error) }
        semaphore.signal()
      }
      _ = semaphore.wait(timeout: .now() + 20)
      switch box.outcome {
      case .success(let renewed):
        try? save(renewed)
        return renewed.accessToken
      case .failure(let error):
        // A rejected refresh token ends the session; a network error keeps it for the next try.
        if case OidcError.sessionExpired = error { clear() }
        return nil
      }
    }
  }
}

/// Carries the refresh result out of the `Task`; read only after the semaphore signalled.
private final class ResultBox: @unchecked Sendable {
  var outcome: Result<OidcSession, Error> = .failure(OidcError.sessionExpired)
}
