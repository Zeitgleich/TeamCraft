import Foundation
import SwiftUI

/// What the sign-in screen shows, top to bottom, for the server's methods: each method once, in the
/// server's order, with "or" between two. Pure, so the layout is tested.
enum SignInSection: Equatable {
  case sso
  case divider
  case local

  static func layout(methods: [SignInOption]) -> [SignInSection] {
    var seen: [SignInOption] = []
    for method in methods where !seen.contains(method) {
      seen.append(method)
    }
    var sections: [SignInSection] = []
    for method in seen {
      if !sections.isEmpty { sections.append(.divider) }
      switch method {
      case .sso: sections.append(.sso)
      case .local: sections.append(.local)
      }
    }
    // The core never hands over an empty list; SSO is its fallback too.
    return sections.isEmpty ? [.sso] : sections
  }
}

/// Why a local sign-in failed, as the screen shows it. One answer for a wrong e-mail address,
/// password or one-time code, so the screen tells nobody which accounts exist (security.md).
enum LocalSignInFailure: Equatable {
  case wrongCredentials
  /// Local accounts are switched off or not set up on the server.
  case disabled
  case rateLimited
  case unreachable
  case generic

  var message: LocalizedStringKey {
    switch self {
    case .wrongCredentials: return "sign_in_error_credentials"
    case .disabled: return "sign_in_error_disabled"
    case .rateLimited: return "sign_in_error_rate_limited"
    case .unreachable: return "sign_in_error_unreachable"
    case .generic: return "sign_in_failed"
    }
  }
}

/// The server's answer to `POST /api/v1/auth/local/login`.
enum LocalSignInResult: Equatable {
  case signedIn(LocalSession)
  case failed(LocalSignInFailure)

  /// Maps an HTTP answer as received; the error code decides before the status.
  static func from(status: Int, body: Data) -> LocalSignInResult {
    if status == 200 {
      guard let granted = try? JSONDecoder().decode(LocalLoginResponse.self, from: body), !granted.accessToken.isEmpty,
        !granted.userId.isEmpty
      else { return .failed(.generic) }
      return .signedIn(
        LocalSession(
          accessToken: granted.accessToken,
          expiresAt: Date(timeIntervalSince1970: TimeInterval(granted.expiresAtMillis) / 1000),
          userId: granted.userId))
    }
    let maybeCode = (try? JSONDecoder().decode(ErrorAnswer.self, from: body))?.errorCode
    switch maybeCode {
    // The server sends no "mfa_required" (a missing code is a plain 401); should one come, it is the
    // same answer as a wrong code.
    case "unauthorized", "mfa_required": return .failed(.wrongCredentials)
    case "local_accounts_disabled": return .failed(.disabled)
    case "rate_limited": return .failed(.rateLimited)
    default: break
    }
    switch status {
    case 401: return .failed(.wrongCredentials)
    // 404: a server without the route; 503: local accounts not configured.
    case 403, 404, 503: return .failed(.disabled)
    case 429: return .failed(.rateLimited)
    default: return .failed(.generic)
    }
  }
}

/// The local sign-in form's input. Pure, so what may be sent is tested.
struct LocalSignInForm: Equatable {
  static let codeLength = 6

  var email = ""
  var password = ""
  var code = ""
  var isPasswordVisible = false
  var maybeFailure: LocalSignInFailure?

  var trimmedEmail: String {
    email.trimmingCharacters(in: .whitespacesAndNewlines)
  }

  /// E-mail and password given, and a complete code when the server requires one (an optional code
  /// is sent only when complete).
  func canSubmit(requiresCode: Bool) -> Bool {
    guard !trimmedEmail.isEmpty, !password.isEmpty else { return false }
    if code.isEmpty { return !requiresCode }
    return code.count == Self.codeLength
  }

  /// The code to send; `nil` when none was entered.
  var maybeCode: String? {
    code.isEmpty ? nil : code
  }

  /// Shows why the attempt failed. The code is used up either way, so it goes; after a wrong
  /// answer the password goes too (the e-mail address stays for the next try).
  mutating func apply(_ failure: LocalSignInFailure) {
    maybeFailure = failure
    code = ""
    if failure == .wrongCredentials { password = "" }
  }

  /// ASCII digits only, at most `codeLength` (pasted codes may carry spaces).
  static func sanitizedCode(_ raw: String) -> String {
    String(raw.filter { $0.isASCII && $0.isNumber }.prefix(codeLength))
  }
}

/// `POST /api/v1/auth/local/login` and `/logout` against the account's server: ephemeral session
/// (no cookies, no cache), bounded timeouts, redirects refused so the password goes nowhere else.
/// Never logs the password or the token.
struct LocalAuthClient {
  private static let maxBodyBytes = 64 * 1024

  private static let session: URLSession = {
    let configuration = URLSessionConfiguration.ephemeral
    configuration.timeoutIntervalForRequest = 15
    configuration.timeoutIntervalForResource = 30
    configuration.requestCachePolicy = .reloadIgnoringLocalCacheData
    configuration.httpShouldSetCookies = false
    return URLSession(configuration: configuration)
  }()

  func login(origin: URL, email: String, password: String, maybeCode: String?) async -> LocalSignInResult {
    var request = URLRequest(url: origin.appending(path: "api/v1/auth/local/login"))
    request.httpMethod = "POST"
    request.setValue("application/json", forHTTPHeaderField: "Content-Type")
    request.setValue("application/json", forHTTPHeaderField: "Accept")
    let body = LocalLoginRequestBody(login: email, password: password, totpCode: maybeCode)
    guard let encoded = try? JSONEncoder().encode(body) else { return .failed(.generic) }
    request.httpBody = encoded
    do {
      let (data, response) = try await Self.session.data(for: request, delegate: RedirectRefuser())
      guard let http = response as? HTTPURLResponse, data.count <= Self.maxBodyBytes else { return .failed(.generic) }
      return LocalSignInResult.from(status: http.statusCode, body: data)
    } catch {
      // Offline, refused, timed out, TLS: nothing reached the account.
      return .failed(.unreachable)
    }
  }

  /// Ends the session on the server. Best effort: offline, the token simply expires.
  func logout(origin: URL, accessToken: String) async {
    var request = URLRequest(url: origin.appending(path: "api/v1/auth/local/logout"))
    request.httpMethod = "POST"
    request.setValue("Bearer \(accessToken)", forHTTPHeaderField: "Authorization")
    _ = try? await Self.session.data(for: request, delegate: RedirectRefuser())
  }
}

/// The sign-in page of the account's server, fresh from `GET /api/v1/server-info` (an admin may
/// change it at any time); "Sign in with <brand>" only when the server can't be asked.
struct SignInPageLoader {
  let client: ServerInfoClient

  func load(address: ServerAddress, brandName: String) async -> SignInPageInfo {
    let language = Locale.preferredLanguages.first ?? "en"
    guard let url = URL(string: address.serverInfoUrl), let answer = try? await client.fetch(url), answer.status == 200,
      let page = signInPage(
        responseJson: String(decoding: answer.body, as: UTF8.self), origin: address.origin, brandName: brandName,
        language: language)
    else { return ssoOnlySignInPage(brandName: brandName) }
    return page
  }
}

/// The body of `POST /api/v1/auth/local/login`; `totp_code` is left out when `nil`.
struct LocalLoginRequestBody: Encodable, Equatable {
  let login: String
  let password: String
  let totpCode: String?

  enum CodingKeys: String, CodingKey {
    case login, password
    case totpCode = "totp_code"
  }
}

/// `LocalLoginResponse` of the server. `user_id` is a Snowflake sent as a string; a number is
/// accepted too.
struct LocalLoginResponse: Decodable, Equatable {
  let accessToken: String
  let expiresAtMillis: Int64
  let userId: String

  enum CodingKeys: String, CodingKey {
    case accessToken = "access_token"
    case expiresAtMillis = "expires_at"
    case userId = "user_id"
  }

  init(from decoder: Decoder) throws {
    let container = try decoder.container(keyedBy: CodingKeys.self)
    accessToken = try container.decode(String.self, forKey: .accessToken)
    expiresAtMillis = try container.decode(Int64.self, forKey: .expiresAtMillis)
    if let text = try? container.decode(String.self, forKey: .userId) {
      userId = text
    } else {
      userId = String(try container.decode(Int64.self, forKey: .userId))
    }
  }
}

/// The server's error body (`ErrorBody`).
private struct ErrorAnswer: Decodable {
  let errorCode: String

  enum CodingKeys: String, CodingKey {
    case errorCode = "error_code"
  }
}
