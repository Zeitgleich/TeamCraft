import Foundation

/// The signed-in session: tokens of the OIDC login. Kept in the shared keychain (never in
/// UserDefaults) so the extension can fetch a message with it while the app is closed.
struct OidcSession: Codable, Equatable {
  var accessToken: String
  var refreshToken: String?
  /// The newest ID token of the session, for the core's role signing (docs/e2ee.md §3.1.3); `nil` for
  /// a sign-in stored before the app kept it, until the first refresh that brings one.
  var idToken: String?
  /// When the access token stops being valid.
  var expiresAt: Date
  /// The OIDC subject: the account key for local data.
  var subject: String
  var displayName: String?
  /// Endpoint and client, so a refresh needs neither discovery nor the server profile.
  var tokenEndpoint: URL
  var clientId: String
  /// The account the ID token names (`preferred_username`, else `email`), for the next sign-in's
  /// login page (issue #448); not stored with the session's tokens elsewhere.
  var loginHint: String?
  /// The device key the session is bound to (`cnf.jkt` of its access token, docs/e2ee.md §3.1.2);
  /// `nil` for a session that isn't bound, as all were before the app knew DPoP. A bound refresh token
  /// needs a proof with this key and no other.
  var boundKey: String?

  /// Valid for at least `leeway` more seconds.
  func isFresh(leeway: TimeInterval = 30, now: Date = Date()) -> Bool {
    expiresAt.timeIntervalSince(now) > leeway
  }

  /// This session after a refresh answered with `response`. An answer without a usable ID token
  /// leaves none: what the core signs with is the newest token or nothing, as on Android.
  func renewed(by response: TokenResponse, now: Date = Date()) -> OidcSession {
    var renewed = self
    renewed.accessToken = response.accessToken
    // The provider may rotate the refresh token; keep the old one if it doesn't.
    renewed.refreshToken = response.refreshToken ?? refreshToken
    renewed.expiresAt = now.addingTimeInterval(response.expiresIn ?? 300)
    renewed.idToken = response.idToken.flatMap { isOwnIdToken($0) ? $0 : nil }
    renewed.boundKey = tokenBoundKey(accessToken: response.accessToken)
    return renewed
  }

  /// Whether a refresh's ID token speaks for this session (OIDC Core §12.2): same subject, this
  /// client, and the issuer of the token before it, if there is one to compare with.
  private func isOwnIdToken(_ candidate: String) -> Bool {
    guard let claims = IdTokenClaims(jwt: candidate), claims.subject == subject,
      claims.audiences.contains(clientId)
    else { return false }
    guard let idToken, let previous = IdTokenClaims(jwt: idToken) else { return true }
    return claims.issuer == previous.issuer
  }
}

// security.md: never print the tokens, also not by accident through `String(describing:)`.
extension OidcSession: CustomStringConvertible, CustomDebugStringConvertible {
  var description: String { "OidcSession([redacted])" }
  var debugDescription: String { description }
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
  /// The identity provider's `error` in the sign-in redirect, cleaned by the core (issue #1063).
  case providerRefused(code: String)
  /// The session is bound to a device key that is gone: nothing went to the provider.
  case deviceKeyLost
  /// The session's device key can't be used right now: nothing went to the provider, nothing is
  /// decided about the session.
  case deviceKeyUnavailable

  /// Whether this is about the device key and not the provider's answer.
  var isDeviceKeyFailure: Bool {
    self == .deviceKeyLost || self == .deviceKeyUnavailable
  }
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
    /// What the provider takes as DPoP proofs (RFC 9449 §5.1).
    var dpopSigningAlgValuesSupported: [String]?

    enum CodingKeys: String, CodingKey {
      case issuer
      case authorizationEndpoint = "authorization_endpoint"
      case tokenEndpoint = "token_endpoint"
      case dpopSigningAlgValuesSupported = "dpop_signing_alg_values_supported"
    }

    /// The provider advertises DPoP with the one algorithm this app signs: ES256.
    var takesProofs: Bool { dpopSigningAlgValuesSupported?.contains("ES256") == true }

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

  func exchange(
    code: String, verifier: String, endpoints: Endpoints, proof: TokenProof = .none
  ) async throws -> OidcSession {
    let response = try await Self.tokenRequest(
      endpoints.tokenEndpoint,
      [
        "grant_type": "authorization_code",
        "code": code,
        "redirect_uri": redirectURI.absoluteString,
        "client_id": clientId,
        "code_verifier": verifier,
      ],
      urlSession: urlSession, proof: proof)
    guard let idToken = response.idToken, let claims = IdTokenClaims(jwt: idToken),
      claims.isIssued(by: issuer.absoluteString, to: clientId)
    else {
      throw OidcError.invalidResponse
    }
    return OidcSession(
      accessToken: response.accessToken, refreshToken: response.refreshToken, idToken: idToken,
      expiresAt: Date().addingTimeInterval(response.expiresIn ?? 300), subject: claims.subject,
      displayName: claims.name, tokenEndpoint: endpoints.tokenEndpoint, clientId: clientId,
      loginHint: loginHintFromIdToken(idToken: idToken), boundKey: tokenBoundKey(accessToken: response.accessToken))
  }

  /// Renews the access token with the session's own endpoint and client.
  static func refresh(
    _ session: OidcSession, urlSession: URLSession = .shared, proof: TokenProof = .none
  ) async throws -> OidcSession {
    guard let refreshToken = session.refreshToken else { throw OidcError.sessionExpired }
    let response = try await tokenRequest(
      session.tokenEndpoint,
      [
        "grant_type": "refresh_token",
        "refresh_token": refreshToken,
        "client_id": session.clientId,
      ],
      urlSession: urlSession, proof: proof)
    return session.renewed(by: response)
  }

  private static func tokenRequest(
    _ endpoint: URL, _ form: [String: String], urlSession: URLSession, proof: TokenProof
  ) async throws -> TokenResponse {
    var request = URLRequest(url: endpoint)
    request.httpMethod = "POST"
    request.timeoutInterval = 15
    request.setValue("application/x-www-form-urlencoded", forHTTPHeaderField: "Content-Type")
    request.httpBody = Self.formEncoded(form).data(using: .utf8)
    do {
      let transport = TokenRequestTransport(request: request, urlSession: urlSession)
      let answer: TokenEndpointAnswer
      switch proof {
      case .none:
        answer = try await transport.send(proof: nil)
      case .with(let prover, let fallback):
        answer = try await prover.tokenRequest(
          endpoint: endpoint.absoluteString, fallback: fallback, transport: transport)
      }
      switch answer.status {
      case 200: return try JSONDecoder().decode(TokenResponse.self, from: answer.body)
      // Only a refused grant (an expired, revoked or reused refresh token) ends the session; any
      // other answer keeps it for the next try (issue #667).
      case 400 where Self.isInvalidGrant(answer.body): throw OidcError.sessionExpired
      default: throw OidcError.tokenRequestFailed(status: Int(answer.status))
      }
    } catch let error as OidcError {
      throw error
    } catch DeviceProofError.KeyLost {
      throw OidcError.deviceKeyLost
    } catch DeviceProofError.KeyUnavailable {
      throw OidcError.deviceKeyUnavailable
    } catch DeviceProofError.NoAnswer(let message) {
      throw OidcError.network(message)
    } catch {
      throw OidcError.network(error.localizedDescription)
    }
  }

  /// Whether a token endpoint error answer says `invalid_grant` (RFC 6749 §5.2).
  static func isInvalidGrant(_ data: Data) -> Bool {
    struct ErrorAnswer: Decodable { let error: String }
    return (try? JSONDecoder().decode(ErrorAnswer.self, from: data))?.error == "invalid_grant"
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

/// Keeps the session of the active server's account in the shared keychain (one item per server:
/// `oidc-session.<storage key>` for SSO, `local-session.<storage key>` for a local account; at most
/// one of the two exists) and hands the core access tokens, refreshing OIDC ones when needed. Used by
/// both processes; refreshes run under the cross-process lock's protection by the callers (the
/// engine only asks while its process holds the lock).
final class SessionVault {
  private static let accountPrefix = "oidc-session."
  private static let localAccountPrefix = "local-session."

  /// How long a process waits for the other one's refresh before it gives up for now.
  private static let refreshLockTimeout: TimeInterval = 20

  private let keychain: any SecretItems
  private let servers: ServerStore
  private let refreshLockURL: URL?
  private let refreshTimeout: TimeInterval
  private let refresh: @Sendable (OidcSession) async throws -> OidcSession
  private let queue = DispatchQueue(label: "de.zeitgleich.teamcraft.session")
  /// What token requests do about DPoP; what the sign-in and the core sign with.
  let proofs: SessionProofs

  /// `bindsSessions`: this process may make a device key and bind a session that isn't bound yet.
  /// The app does, the notification extension only signs for sessions that are bound already.
  init(
    keychain: any SecretItems = KeychainStore(), servers: ServerStore = ServerStore(),
    refreshLockURL: URL? = SharedContainer().refreshLockFileURL,
    refreshTimeout: TimeInterval = 20,
    bindsSessions: Bool = false,
    proofs: SessionProofs? = nil,
    urlSession: URLSession = .shared,
    refresh: (@Sendable (OidcSession) async throws -> OidcSession)? = nil
  ) {
    self.keychain = keychain
    self.servers = servers
    self.refreshLockURL = refreshLockURL
    self.refreshTimeout = refreshTimeout
    let proofs =
      proofs
      ?? SessionProofs(
        keys: DeviceKeyStore(keychain: keychain, servers: servers), servers: servers, bindsSessions: bindsSessions)
    self.proofs = proofs
    self.refresh = refresh ?? { try await proofs.refresh($0, urlSession: urlSession) }
  }

  func load() -> OidcSession? {
    guard let account = activeItem, let data = try? keychain.read(account) else { return nil }
    return try? JSONDecoder().decode(OidcSession.self, from: data)
  }

  func save(_ session: OidcSession) throws {
    guard let account = activeItem, let localAccount = activeLocalItem else { throw SessionVaultError.noServer }
    try keychain.write(JSONEncoder().encode(session), for: account)
    // One session per server: a local one from before goes.
    try? keychain.delete(localAccount)
  }

  func loadLocal() -> LocalSession? {
    guard let account = activeLocalItem, let data = try? keychain.read(account) else { return nil }
    return try? JSONDecoder().decode(LocalSession.self, from: data)
  }

  func saveLocal(_ session: LocalSession) throws {
    guard let account = activeItem, let localAccount = activeLocalItem else { throw SessionVaultError.noServer }
    try keychain.write(JSONEncoder().encode(session), for: localAccount)
    // One session per server: an OIDC one from before goes, and the key it was bound to with it.
    try? keychain.delete(account)
    forgetDeviceKey()
  }

  /// Whether the active server has a session of either kind (it may have expired).
  var hasSession: Bool {
    load() != nil || loadLocal() != nil
  }

  func clear() {
    for account in [activeItem, activeLocalItem].compactMap({ $0 }) {
      // Nothing to do about a failed delete; the next sign-in overwrites the item.
      try? keychain.delete(account)
    }
    forgetDeviceKey()
  }

  /// The active server's device key goes with its session (sign-out, or the provider refused the
  /// refresh token), so that the next sign-in starts with a new one (docs/e2ee.md §3.1.2).
  private func forgetDeviceKey() {
    proofs.keys.delete()
    proofs.forgetKey()
  }

  /// What the Rust core signs with: the active server's device key, read anew for each use.
  var deviceKey: DeviceProofKey {
    VaultDeviceKey(store: proofs.keys)
  }

  /// Whether the session is bound to a device key that is gone (never written, or removed): it can't
  /// be used any more and the user has to sign in again. A key that just can't be read is no loss.
  /// Reads the keychain; safe on the main thread.
  var isDeviceKeyLost: Bool {
    guard let bound = load()?.boundKey else { return false }
    do {
      return try ProviderProver(key: deviceKey).thumbprint() != bound
    } catch DeviceProofError.KeyLost {
      return true
    } catch {
      return false
    }
  }

  /// A valid access token, refreshed if it is about to expire; `nil` when signed out or the
  /// refresh failed. Blocks (up to the request timeout): the core asks on its engine thread.
  ///
  /// `forceRefresh` renews a token that is still good, for a newer ID token. That is one attempt:
  /// if it fails the token still is good and comes back, and the next regular refresh brings the
  /// ID token instead.
  func currentAccessToken(forceRefresh: Bool = false) -> String? {
    queue.sync {
      if let local = loadLocal() {
        // No refresh for local sessions: past its expiry the account is signed out.
        return local.isUsable() ? local.accessToken : nil
      }
      guard let stale = load() else { return nil }
      if stale.isFresh(), !forceRefresh { return stale.accessToken }
      // One refresh across the app and the notification extension (issue #667): whoever waited
      // reads the keychain again and takes the token the other one got.
      let lock = refreshLockURL.map { ProcessLock(fileURL: $0) }
      if let lock, !lock.acquire(timeout: Self.refreshLockTimeout) { return stale.accessTokenIfFresh }
      defer { lock?.release() }
      guard let session = load() else { return nil }
      // The other process's refresh brought a new ID token too: no second one for a forced refresh.
      if session.isFresh(), !forceRefresh || session.accessToken != stale.accessToken {
        return session.accessToken
      }
      guard session.refreshToken != nil else { return session.accessTokenIfFresh }
      let semaphore = DispatchSemaphore(value: 0)
      let box = ResultBox()
      let refresh = self.refresh
      Task.detached {
        do { box.outcome = .success(try await refresh(session)) } catch { box.outcome = .failure(error) }
        semaphore.signal()
      }
      _ = semaphore.wait(timeout: .now() + refreshTimeout)
      switch box.outcome {
      case .success(let renewed):
        try? save(renewed)
        return renewed.accessToken
      case .failure(let error):
        switch error {
        // A rejected refresh token ends the session.
        case OidcError.sessionExpired:
          clear()
          return nil
        // A device key that is gone or can't be used: nothing went to the provider. The token comes
        // back as it is, also an expired one: the core sees that it is bound, holds it back and
        // says why. `nil` would read as signed out.
        case OidcError.deviceKeyLost, OidcError.deviceKeyUnavailable:
          return session.accessToken
        // Anything else keeps the session for the next try.
        default:
          return session.accessTokenIfFresh
        }
      }
    }
  }

  /// The session's newest ID token; `nil` when signed out, for a local account, or while the
  /// provider has sent none. Only reads the keychain and never waits for a refresh, so it is safe
  /// on the main thread while the core's thread is in `currentAccessToken`.
  func currentIdToken() -> String? {
    load()?.idToken
  }

  private var activeItem: String? {
    servers.active.map { Self.accountPrefix + $0.storageKey }
  }

  private var activeLocalItem: String? {
    servers.active.map { Self.localAccountPrefix + $0.storageKey }
  }
}

extension OidcSession {
  fileprivate var accessTokenIfFresh: String? { isFresh() ? accessToken : nil }
}

/// Carries the refresh result out of the `Task`; read after the semaphore signalled or timed out.
/// A refresh that didn't finish in time is no answer from the provider, so it must not read as a
/// rejected refresh token, which ends the session.
private final class ResultBox: @unchecked Sendable {
  var outcome: Result<OidcSession, Error> = .failure(OidcError.network("timed out"))
}
