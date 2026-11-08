import Foundation
import os

/// What a token request does about DPoP (docs/e2ee.md §3.1.2).
enum TokenProof {
  /// It goes without one: the session isn't bound and nothing says it should be.
  case none
  /// With the device's key. `fallback` says when it goes without after all.
  case with(prover: ProviderProver, fallback: TokenProofFallback)
}

/// What a fresh `GET /server-info` says about proofs.
enum ServerProofOffer: Equatable {
  /// It has `DPoP-Nonce`: the server would check them on this host.
  case offered
  /// It has none, or the server said it has no such page (404) or doesn't know the host (421).
  case notOffered
  /// No answer, or an error status: that says nothing about proofs.
  case noAnswer

  init(status: Int, maybeNonce: String?) {
    switch status {
    case 200...299: self = maybeNonce == nil ? .notOffered : .offered
    case 404, 421: self = .notOffered
    default: self = .noAnswer
    }
  }
}

/// The decisions about proofs for token requests (docs/e2ee.md §3.1.2): the core signs and handles
/// retries; this says when a proof goes at all. One per vault, so that the provider's nonce and clock
/// are remembered between refreshes.
final class SessionProofs: @unchecked Sendable {
  private static let log = Logger(subsystem: "de.zeitgleich.teamcraft", category: "sign-in")
  /// One more try for a server that doesn't answer before a sign-in fails for it.
  private static let serverAskRetry: UInt64 = 500_000_000

  let keys: DeviceKeyStore
  private let activeServer: @Sendable () -> StoredServer?
  private let bindsSessions: Bool
  private let askServer: @Sendable (StoredServer) async -> ServerProofOffer
  private let providerTakesProofs: @Sendable (StoredServer) async -> Bool?
  private let lock = NSLock()
  /// The provider's prover and the server whose key it is: a server switched to has its own.
  private var maybeProvider: (owner: String, prover: ProviderProver)?
  private var maybeKnownProvider: (issuer: String, takesProofs: Bool)?

  /// `bindsSessions`: this process may make a key and bind a session that isn't bound yet. The app
  /// does; the notification extension only signs for sessions that are bound already.
  init(
    keys: DeviceKeyStore, servers: ServerStore, bindsSessions: Bool,
    askServer: @escaping @Sendable (StoredServer) async -> ServerProofOffer = { await ServerProofProbe().ask($0) },
    providerTakesProofs: @escaping @Sendable (StoredServer) async -> Bool? = SessionProofs.discoverProofs,
    activeServer: (@Sendable () -> StoredServer?)? = nil
  ) {
    self.keys = keys
    self.activeServer = activeServer ?? { servers.activeServer }
    self.bindsSessions = bindsSessions
    self.askServer = askServer
    self.providerTakesProofs = providerTakesProofs
  }

  /// The proof of a new sign-in: with one only if the provider takes them and the server would check
  /// them. Otherwise, or when no key can be kept, the session stays unbound and nobody is locked
  /// out. A server that doesn't answer fails the sign-in, like the desktop: it says nothing either way.
  func forSignIn(endpoints: OidcClient.Endpoints, server: StoredServer) async throws -> TokenProof {
    guard endpoints.takesProofs else { return .none }
    switch await askServerPatiently(server) {
    case .offered: break
    case .notOffered: return .none
    case .noAnswer: throw OidcError.network("the server doesn't answer whether it checks proofs")
    }
    return newKeyProof(isNewSignIn: true)
  }

  /// The proof of a refresh. A bound session gets one with its key, whatever discovery says by now,
  /// or nothing goes to the provider: a key that is gone or can't be read throws. An unbound one
  /// is worth binding in the app, when both sides would do it (migration of sessions from before).
  func forRefresh(of session: OidcSession) async throws -> TokenProof {
    if let bound = session.boundKey {
      return .with(prover: try prover(boundTo: bound), fallback: .none)
    }
    guard bindsSessions, let server = activeServer() else { return .none }
    if knownProver?.hasRefusedProof() == true { return .none }
    guard await knownProviderTakesProofs(server) == true, await askServer(server) == .offered else {
      return .none
    }
    return newKeyProof(isNewSignIn: false)
  }

  /// The refresh of `session` with the proof it calls for. A key made to bind an unbound session
  /// that then can't sign goes (nothing was spent: the signature comes before the request), and the
  /// refresh goes on as it always did.
  func refresh(_ session: OidcSession, urlSession: URLSession) async throws -> OidcSession {
    let proof = try await forRefresh(of: session)
    do {
      return try await OidcClient.refresh(session, urlSession: urlSession, proof: proof)
    } catch let error as OidcError where session.boundKey == nil && error.isDeviceKeyFailure {
      discardUnusableKey()
      return try await OidcClient.refresh(session, urlSession: urlSession)
    }
  }

  /// The code exchange of a new sign-in with `proof`. A key that can't sign doesn't cost the sign-in:
  /// nothing was spent yet, the key goes so that the next sign-in makes a new one, and this
  /// session stays unbound.
  func exchange(
    with client: OidcClient, code: String, verifier: String, endpoints: OidcClient.Endpoints, proof: TokenProof
  ) async throws -> OidcSession {
    do {
      return try await client.exchange(code: code, verifier: verifier, endpoints: endpoints, proof: proof)
    } catch let error as OidcError where error.isDeviceKeyFailure {
      discardUnusableKey()
      return try await client.exchange(code: code, verifier: verifier, endpoints: endpoints)
    }
  }

  /// The key went with its session: nothing that was learned about the provider is kept for it.
  func forgetKey() {
    lock.withLock { maybeProvider = nil }
  }

  private func discardUnusableKey() {
    Self.log.warning("the device key couldn't sign, the session stays unbound")
    keys.delete()
    forgetKey()
  }

  private func newKeyProof(isNewSignIn: Bool) -> TokenProof {
    do {
      _ = try keys.keyOrNew()
      let prover = try prover(reusing: !isNewSignIn)
      return .with(prover: prover, fallback: .onRefusal)
    } catch {
      Self.log.warning("no device key could be kept, the session stays unbound")
      return .none
    }
  }

  private func prover(reusing: Bool) throws -> ProviderProver {
    if reusing, let known = knownProver { return known }
    return try newProver()
  }

  /// The prover of a session bound to `bound`. A key that isn't this one is as lost as none.
  private func prover(boundTo bound: String) throws -> ProviderProver {
    if let known = knownProver, known.thumbprint() == bound { return known }
    let prover: ProviderProver
    do {
      prover = try newProver()
    } catch DeviceProofError.KeyLost {
      throw OidcError.deviceKeyLost
    } catch {
      throw OidcError.deviceKeyUnavailable
    }
    guard prover.thumbprint() == bound else { throw OidcError.deviceKeyLost }
    return prover
  }

  /// What is known about the provider, for the active server's key.
  private var knownProver: ProviderProver? {
    let owner = keys.activeServerKey
    return lock.withLock { maybeProvider?.owner == owner ? maybeProvider?.prover : nil }
  }

  private func newProver() throws -> ProviderProver {
    let prover = try ProviderProver(key: VaultDeviceKey(store: keys))
    if let owner = keys.activeServerKey {
      lock.withLock { maybeProvider = (owner, prover) }
    }
    return prover
  }

  private func askServerPatiently(_ server: StoredServer) async -> ServerProofOffer {
    let first = await askServer(server)
    guard first == .noAnswer else { return first }
    try? await Task.sleep(nanoseconds: Self.serverAskRetry)
    return await askServer(server)
  }

  /// Discovery once per process and issuer; an answer that isn't there is asked again next time.
  private func knownProviderTakesProofs(_ server: StoredServer) async -> Bool? {
    if let known = lock.withLock({ maybeKnownProvider }), known.issuer == server.issuer { return known.takesProofs }
    guard let takesProofs = await providerTakesProofs(server) else { return nil }
    lock.withLock { maybeKnownProvider = (server.issuer, takesProofs) }
    return takesProofs
  }

  /// Whether the provider's discovery document names ES256; `nil` when it can't be read.
  static func discoverProofs(_ server: StoredServer) async -> Bool? {
    guard let client = OidcClient(server: server), let endpoints = try? await client.discover() else { return nil }
    return endpoints.takesProofs
  }
}

/// Asks the active server whether it would check proofs: past any cache, no redirects followed.
struct ServerProofProbe {
  private static let session: URLSession = {
    let configuration = URLSessionConfiguration.ephemeral
    configuration.timeoutIntervalForRequest = 15
    configuration.timeoutIntervalForResource = 30
    configuration.requestCachePolicy = .reloadIgnoringLocalCacheData
    configuration.httpShouldSetCookies = false
    return URLSession(configuration: configuration)
  }()

  func ask(_ server: StoredServer) async -> ServerProofOffer {
    guard let url = URL(string: server.address.serverInfoUrl) else { return .noAnswer }
    var request = URLRequest(url: url)
    request.setValue("no-store", forHTTPHeaderField: "Cache-Control")
    guard let (_, response) = try? await Self.session.data(for: request, delegate: RedirectRefuser()),
      let http = response as? HTTPURLResponse
    else { return .noAnswer }
    return ServerProofOffer(status: http.statusCode, maybeNonce: http.value(forHTTPHeaderField: "DPoP-Nonce"))
  }
}

/// One request to a token endpoint over `URLSession`. With a proof it follows no redirect: the proof
/// names the first URL, and `URLSession` would carry it along to the next one.
final class TokenRequestTransport: TokenEndpointTransport, @unchecked Sendable {
  private let request: URLRequest
  private let urlSession: URLSession

  init(request: URLRequest, urlSession: URLSession) {
    self.request = request
    self.urlSession = urlSession
  }

  func send(proof: String?) async throws -> TokenEndpointAnswer {
    var request = self.request
    var maybeDelegate: URLSessionTaskDelegate?
    if let proof {
      request.setValue(proof, forHTTPHeaderField: "DPoP")
      maybeDelegate = RedirectRefuser()
    }
    do {
      let (data, response) = try await urlSession.data(for: request, delegate: maybeDelegate)
      let http = response as? HTTPURLResponse
      return TokenEndpointAnswer(
        status: UInt16(clamping: http?.statusCode ?? 0), body: data,
        nonce: http?.value(forHTTPHeaderField: "DPoP-Nonce"), date: http?.value(forHTTPHeaderField: "Date"))
    } catch {
      throw DeviceProofError.NoAnswer(message: error.localizedDescription)
    }
  }
}
