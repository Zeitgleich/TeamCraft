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
  /// Endpoint and client, so a refresh needs neither discovery nor the server profile.
  var tokenEndpoint: URL
  var clientId: String

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
  let issuer: String?
  /// `aud` as a list (a single string becomes one entry).
  let audiences: [String]

  init?(jwt: String) {
    let parts = jwt.split(separator: ".")
    guard parts.count >= 2,
      let payload = PushPayload.decodeBase64URL(String(parts[1])),
      let json = try? JSONSerialization.jsonObject(with: payload) as? [String: Any],
      let subject = json["sub"] as? String
    else { return nil }
    self.subject = subject
    self.name = (json["name"] as? String) ?? (json["preferred_username"] as? String)
    self.issuer = json["iss"] as? String
    if let audience = json["aud"] as? String {
      self.audiences = [audience]
    } else {
      self.audiences = (json["aud"] as? [String]) ?? []
    }
  }

  /// Whether the token was issued by `issuer` to `clientId` (OIDC Core §3.1.3.7, steps 2 and 3).
  func isIssued(by issuer: String, to clientId: String) -> Bool {
    self.issuer == issuer && audiences.contains(clientId)
  }
}

/// Discovery, code exchange and refresh against the issuer (RFC 6749, OIDC Core, PKCE RFC 7636).
/// Issuer, client and scopes come from the server's checked profile (`StoredServer`). The browser
/// part of the login lives in the app target (`WebLogin`).
struct OidcClient {
  let issuer: URL
  let clientId: String
  let scopes: [String]
  let redirectURI: URL
  var urlSession: URLSession = .shared

  struct Endpoints: Decodable, Equatable {
    /// The document's `issuer`; must equal the configured one (RFC 8414 §3.3).
    var issuer: String?
    let authorizationEndpoint: URL
    let tokenEndpoint: URL

    enum CodingKeys: String, CodingKey {
      case issuer
      case authorizationEndpoint = "authorization_endpoint"
      case tokenEndpoint = "token_endpoint"
    }

    /// https only (plain http to this device in debug builds), and the issuer's own document.
    func isTrusted(for issuer: URL) -> Bool {
      self.issuer == issuer.absoluteString
        && [authorizationEndpoint, tokenEndpoint].allSatisfy(Self.isSafe)
    }

    static func isSafe(_ url: URL) -> Bool {
      guard url.user == nil, url.password == nil, let host = url.host, !host.isEmpty else { return false }
      switch url.scheme?.lowercased() {
      case "https": return true
      case "http":
        #if DEBUG
          return true
        #else
          return ["localhost", "127.0.0.1", "::1"].contains(host)
        #endif
      default: return false
      }
    }
  }

  func discover() async throws -> Endpoints {
    let url = issuer.appendingPathComponent(".well-known/openid-configuration")
    do {
      let (data, response) = try await urlSession.data(from: url)
      guard (response as? HTTPURLResponse)?.statusCode == 200 else { throw OidcError.discoveryFailed }
      let endpoints = try JSONDecoder().decode(Endpoints.self, from: data)
      guard endpoints.isTrusted(for: issuer) else { throw OidcError.discoveryFailed }
      return endpoints
    } catch let error as OidcError {
      throw error
    } catch {
      throw OidcError.network(error.localizedDescription)
    }
  }

  func exchange(code: String, verifier: String, endpoints: Endpoints) async throws -> OidcSession {
    let response = try await Self.tokenRequest(
      endpoints.tokenEndpoint,
      [
        "grant_type": "authorization_code",
        "code": code,
        "redirect_uri": redirectURI.absoluteString,
        "client_id": clientId,
        "code_verifier": verifier,
      ],
      urlSession: urlSession)
    guard let idToken = response.idToken, let claims = IdTokenClaims(jwt: idToken),
      claims.isIssued(by: issuer.absoluteString, to: clientId)
    else {
      throw OidcError.invalidResponse
    }
    return OidcSession(
      accessToken: response.accessToken, refreshToken: response.refreshToken,
      expiresAt: Date().addingTimeInterval(response.expiresIn ?? 300), subject: claims.subject,
      displayName: claims.name, tokenEndpoint: endpoints.tokenEndpoint, clientId: clientId)
  }

  /// Renews the access token with the session's own endpoint and client.
  static func refresh(_ session: OidcSession, urlSession: URLSession = .shared) async throws -> OidcSession {
    guard let refreshToken = session.refreshToken else { throw OidcError.sessionExpired }
    let response = try await tokenRequest(
      session.tokenEndpoint,
      [
        "grant_type": "refresh_token",
        "refresh_token": refreshToken,
        "client_id": session.clientId,
      ],
      urlSession: urlSession)
    var renewed = session
    renewed.accessToken = response.accessToken
    // The provider may rotate the refresh token; keep the old one if it doesn't.
    renewed.refreshToken = response.refreshToken ?? refreshToken
    renewed.expiresAt = Date().addingTimeInterval(response.expiresIn ?? 300)
    return renewed
  }

  private static func tokenRequest(
    _ endpoint: URL, _ form: [String: String], urlSession: URLSession
  ) async throws -> TokenResponse {
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

extension OidcClient {
  /// The client for `server`'s sign-in settings; `nil` when its issuer is not a URL (the core
  /// checked it, so only a damaged profile gets here).
  init?(server: StoredServer, environment: AppEnvironment = .current) {
    guard let issuer = URL(string: server.issuer) else { return nil }
    self.init(
      issuer: issuer, clientId: server.clientId, scopes: server.scopes,
      redirectURI: environment.oidcRedirectURI)
  }
}

enum SessionVaultError: Error, Equatable {
  /// No active server to keep a session for.
  case noServer
}

/// Keeps the session of the active server's account in the shared keychain (one item per server,
/// `oidc-session.<storage key>`) and hands the core access tokens, refreshing when needed. Used by
/// both processes; refreshes run under the cross-process lock's protection by the callers (the
/// engine only asks while its process holds the lock).
final class SessionVault {
  /// The single item of builds that had one server baked in.
  private static let legacyAccount = "oidc-session"
  private static let accountPrefix = "oidc-session."

  private let keychain: KeychainStore
  private let servers: ServerStore
  private let queue = DispatchQueue(label: "de.zeitgleich.teamcraft.session")

  init(keychain: KeychainStore = KeychainStore(), servers: ServerStore = ServerStore()) {
    self.keychain = keychain
    self.servers = servers
  }

  func load() -> OidcSession? {
    guard let account = activeItem, let data = try? keychain.read(account) else { return nil }
    return try? JSONDecoder().decode(OidcSession.self, from: data)
  }

  func save(_ session: OidcSession) throws {
    guard let account = activeItem else { throw SessionVaultError.noServer }
    try keychain.write(JSONEncoder().encode(session), for: account)
  }

  func clear() {
    guard let account = activeItem else { return }
    try? keychain.delete(account)
  }

  /// Deletes the session of a build before server entry; `true` if there was one (its account
  /// then signs in again).
  func removeLegacySession() -> Bool {
    guard (try? keychain.read(Self.legacyAccount)) != nil else { return false }
    try? keychain.delete(Self.legacyAccount)
    return true
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
      Task.detached {
        do { box.outcome = .success(try await OidcClient.refresh(session)) } catch { box.outcome = .failure(error) }
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

  private var activeItem: String? {
    servers.active.map { Self.accountPrefix + $0.storageKey }
  }
}

/// Carries the refresh result out of the `Task`; read only after the semaphore signalled.
private final class ResultBox: @unchecked Sendable {
  var outcome: Result<OidcSession, Error> = .failure(OidcError.sessionExpired)
}
