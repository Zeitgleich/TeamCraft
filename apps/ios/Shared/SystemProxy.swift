import CFNetwork
import Foundation

/// Hands the system's HTTP proxy to the Rust core, which can't read it itself (issue #157). iOS
/// keeps one proxy for HTTPS (Wi-Fi settings, an MDM profile); PAC scripts aren't evaluated, so
/// with only a PAC URL the core connects directly. Called when a core opens (app and notification
/// extension) and on every network path change.
enum SystemProxy {
  static func report() {
    guard let settings = CFNetworkCopySystemProxySettings()?.takeRetainedValue() as? [String: Any]
    else {
      setNetworkProxy(maybeProxy: nil)
      return
    }
    // The HTTPS keys have no constants on iOS; the dictionary uses the same names as on macOS.
    let isEnabled = (settings["HTTPSEnable"] as? Int) == 1
      || (settings[kCFNetworkProxiesHTTPEnable as String] as? Int) == 1
    let host =
      settings["HTTPSProxy"] as? String
      ?? settings[kCFNetworkProxiesHTTPProxy as String] as? String
    let port =
      settings["HTTPSPort"] as? Int
      ?? settings[kCFNetworkProxiesHTTPPort as String] as? Int
    guard isEnabled, let host, !host.isEmpty, let port, port > 0, port <= Int(UInt16.max) else {
      setNetworkProxy(maybeProxy: nil)
      return
    }
    let exclusions = settings["ExceptionsList"] as? [String] ?? []
    setNetworkProxy(
      maybeProxy: NetworkProxy(host: host, port: UInt16(port), exclusions: exclusions))
  }
}
