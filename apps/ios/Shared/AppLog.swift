import Foundation
import os

/// The app's own lines in the system log (issue #1067). The core logs under the same subsystem
/// (category `TeamCraft`), so `log stream --predicate 'subsystem == "de.zeitgleich.teamcraft"'`
/// shows both.
enum AppLog {
  static let setup = Logger(subsystem: "de.zeitgleich.teamcraft", category: "Setup")

  /// Logs a failed step with the error's type and its description: for a core error the variant
  /// and the core's message (`WrongPassphrase(message: …)`). Neither carries secrets
  /// (security.md), so both are public in the log.
  static func failure(_ step: String, _ error: Error, logger: Logger = setup) {
    let type = String(reflecting: Swift.type(of: error))
    let description = String(describing: error)
    logger.warning(
      "\(step, privacy: .public) failed: \(type, privacy: .public): \(description, privacy: .public)")
  }
}
