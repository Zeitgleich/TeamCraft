import SwiftUI

/// The errors of a form, one per field at most (forms.md, "Errors at the field"). `Field.allCases`
/// lists the fields top to bottom, so `maybeFirst` is the field a refused submit focuses. Pure, so
/// every form's rules are tested.
struct FieldErrors<Field: CaseIterable & Hashable, Problem: Equatable>: Equatable {
  private var problems: [Field: Problem]

  init(_ checks: [Field: Problem?] = [:]) {
    problems = checks.compactMapValues { $0 }
  }

  var isValid: Bool { problems.isEmpty }

  /// The first invalid field top to bottom, or nil when the form can be sent.
  var maybeFirst: Field? { Field.allCases.first { problems[$0] != nil } }

  subscript(field: Field) -> Problem? { problems[field] }

  /// These errors with `maybeProblem` at `field`, where the server refused that field's value.
  func with(_ maybeProblem: Problem?, at field: Field) -> Self {
    guard let maybeProblem else { return self }
    var copy = self
    copy.problems[field] = maybeProblem
    return copy
  }
}

/// A problem a field can show: its message below the field and in its accessibility hint.
protocol FieldProblem: Equatable {
  var message: LocalizedStringKey { get }
}

// MARK: Passphrases

/// The fields of choosing a new recovery passphrase.
enum NewPassphraseField: CaseIterable, Hashable {
  case passphrase, repeated
}

enum PassphraseProblem: FieldProblem {
  case missing, tooShort, repeatMissing, mismatch, wrong

  var message: LocalizedStringKey {
    switch self {
    case .missing: return "passphrase_missing"
    case .tooShort: return "passphrase_too_short"
    case .repeatMissing: return "passphrase_repeat_missing"
    case .mismatch: return "passphrases_differ"
    case .wrong: return "error_wrong_passphrase"
    }
  }
}

extension PassphraseRules {
  /// A new passphrase and its repetition: too short at the first field, a missing or different
  /// repetition at the second, both at once.
  static func errors(passphrase: String, repeated: String) -> FieldErrors<
    NewPassphraseField, PassphraseProblem
  > {
    let repeatProblem: PassphraseProblem? =
      repeated.isEmpty ? .repeatMissing : (repeated == passphrase ? nil : .mismatch)
    return FieldErrors([.passphrase: problem(passphrase: passphrase), .repeated: repeatProblem])
  }

  /// A single passphrase field: empty or shorter than the minimum.
  static func problem(passphrase: String) -> PassphraseProblem? {
    if passphrase.isEmpty { return .missing }
    return passphrase.count < minimumLength ? .tooShort : nil
  }
}

// MARK: Sign-in

/// The fields of the local sign-in form, top to bottom.
enum SignInField: CaseIterable, Hashable {
  case email, password, code
}

enum SignInProblem: FieldProblem {
  case emailMissing, passwordMissing, codeMissing, codeIncomplete
  /// The server refused e-mail, password or code (one answer for all three).
  case refused(LocalSignInFailure)

  var message: LocalizedStringKey {
    switch self {
    case .emailMissing: return "sign_in_email_missing"
    case .passwordMissing: return "sign_in_password_missing"
    case .codeMissing: return "sign_in_code_missing"
    case .codeIncomplete: return "sign_in_code_incomplete"
    case .refused(let failure): return failure.message
    }
  }
}

extension LocalSignInForm {
  /// Why the form can't be sent yet, at each field: an e-mail and a password, and a complete code
  /// when the server requires one (an optional code is either empty or complete).
  func errors(requiresCode: Bool) -> FieldErrors<SignInField, SignInProblem> {
    var codeProblem: SignInProblem?
    if code.isEmpty {
      codeProblem = requiresCode ? .codeMissing : nil
    } else if code.count != Self.codeLength {
      codeProblem = .codeIncomplete
    }
    return FieldErrors([
      .email: trimmedEmail.isEmpty ? .emailMissing : nil,
      .password: password.isEmpty ? .passwordMissing : nil,
      .code: codeProblem,
    ])
  }
}

extension LocalSignInFailure {
  /// The field a failed local sign-in belongs to, or nil when it belongs to none (the server is
  /// unreachable, local accounts are off). A wrong e-mail, password or code share one answer; it
  /// goes to the password, the field the user retypes.
  var maybeField: SignInField? { self == .wrongCredentials ? .password : nil }
}

// MARK: Single fields

/// A field that takes one text: a missing or incomplete value (the key of its message), or the
/// server's refusal of it.
enum TextProblem: FieldProblem {
  case missing(String)
  case incomplete(String)
  case duplicate(String)
  /// The value was checked and is wrong (a code the other device doesn't show).
  case rejected(String)
  case refused(AppFailure)

  var message: LocalizedStringKey {
    switch self {
    case .missing(let key), .incomplete(let key), .duplicate(let key), .rejected(let key):
      return LocalizedStringKey(key)
    case .refused(let failure): return failure.message
    }
  }

  /// A text that may not be blank: a channel name, a status.
  static func required(_ text: String, missing key: String) -> TextProblem? {
    text.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty ? .missing(key) : nil
  }
}

extension LinkCode {
  /// Digits of the code a new device shows while it is linked (rendezvous short code).
  static let length = 6

  /// The code the new device shows, as typed on this one.
  static func problem(typed: String) -> TextProblem? {
    let digits = digits(of: typed)
    if digits.isEmpty { return .missing("link_code_missing") }
    return digits.count == length ? nil : .incomplete("link_code_incomplete")
  }
}

/// A new mention keyword: not blank and not one the user has already.
func keywordProblem(_ keyword: String, existing: [String]) -> TextProblem? {
  let trimmed = keyword.trimmingCharacters(in: .whitespacesAndNewlines)
  if trimmed.isEmpty { return .missing("rules_keyword_missing") }
  let isDuplicate = existing.contains { $0.caseInsensitiveCompare(trimmed) == .orderedSame }
  return isDuplicate ? .duplicate("rules_keyword_duplicate") : nil
}
