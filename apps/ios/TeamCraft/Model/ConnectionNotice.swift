import Foundation

/// What the connection banner says, if anything. The app shows what the local store has at once
/// and reconnects quietly; only a connection that is still down `ConnectionGrace.period` after it
/// went down (or after the app came back to the foreground) gets the banner (issue #713, the same
/// rule as web and desktop #685 and Android #700).
enum ConnectionNotice: Equatable {
  /// The device itself has no network path.
  case deviceOffline
  case connecting
  /// Connected, but the server answers late (a saturated link): messages still go out (#1064).
  case slow
  /// Online, but the server isn't reachable; the next attempt is in this many seconds. Names what
  /// answered instead, if the core found out (a captive portal, a TLS-intercepting proxy).
  case unreachable(retryInSeconds: Int, maybeInterception: NetworkInterception?)
  /// The server refused the session; shown at once, waiting doesn't fix it.
  case signInRequired
  /// The identity provider lists no role for the account any more; an admin has to give one back.
  case noRoleLeft

  /// The banner for this moment, or none. The device's own connectivity wins: without a network
  /// the server can't be the problem. No core open (`nil`): nothing to say about the server.
  static func current(isOnline: Bool, connection maybeConnection: ConnectionState?, grace: ConnectionGrace, now: Date)
    -> ConnectionNotice?
  {
    if case .signInRequired = maybeConnection { return .signInRequired }
    if case .noRoleLeft = maybeConnection { return .noRoleLeft }
    // Connected, so no grace period: the core doesn't flip it back and forth.
    if isOnline, case .slow = maybeConnection { return .slow }
    guard grace.isPastGrace(now: now) else { return nil }
    if !isOnline { return .deviceOffline }
    switch maybeConnection {
    case .connecting:
      return .connecting
    case .offline(let retryAtMs, let maybeInterception):
      let seconds = max(0, Int((Double(retryAtMs) / 1000 - now.timeIntervalSince1970).rounded(.up)))
      // Due but not started yet: "next attempt in 0 s" would read as stuck.
      if seconds == 0, maybeInterception == nil { return .connecting }
      return .unreachable(retryInSeconds: seconds, maybeInterception: maybeInterception)
    case .connected, .slow, .signInRequired, .noRoleLeft, nil:
      return nil
    }
  }

  /// The thin bar along the top edge: red while the app can't reach the server, yellow while it
  /// connects, none once connected. Reconnecting is the app's job, so this is all it says.
  var strip: ConnectionStrip? {
    switch self {
    case .deviceOffline: return .down
    case .unreachable(_, nil): return .down
    case .connecting: return .connecting
    // Connected, only slow: nothing is wrong. What the user must act on is the banner.
    case .unreachable, .slow, .signInRequired, .noRoleLeft: return nil
    }
  }

  /// Whether the user must do something: sign in again, sign in to the network, a certificate.
  var needsBanner: Bool {
    switch self {
    case .signInRequired, .noRoleLeft: return true
    case .unreachable(_, let maybeInterception): return maybeInterception != nil
    case .deviceOffline, .connecting, .slow: return false
    }
  }

  /// Whether the thin "loading new messages" line shows: the device has a network, the connection
  /// is up and the scope still has messages to come (`isCatchingUp`: this conversation's mark in a
  /// conversation, any conversation's in the list; issue #1457, as on Android). Connecting is the
  /// strip's along the top (`ConnectionStripBar`); offline it speaks too.
  static func isSyncing(isOnline: Bool, isCatchingUp: Bool, connection maybeConnection: ConnectionState?) -> Bool {
    guard isOnline, isCatchingUp else { return false }
    if case .connecting = maybeConnection { return false }
    return true
  }

  /// Whether the app counts as not connected: no network, or the core is (re)connecting. Only then
  /// can the banner change with time alone (grace running out, a countdown), so only then does it
  /// need a clock; a connected app doesn't wake up every second.
  static func isDown(isOnline: Bool, connection maybeConnection: ConnectionState?) -> Bool {
    if !isOnline { return true }
    switch maybeConnection {
    case .connecting, .offline: return true
    case .connected, .slow, .signInRequired, .noRoleLeft, nil: return false
    }
  }
}

extension ConnectionState {
  /// Connected, also on a slow link: what needs the server can go ahead.
  var isConnected: Bool {
    switch self {
    case .connected, .slow: return true
    case .connecting, .offline, .signInRequired, .noRoleLeft: return false
    }
  }
}

/// The connection strip's colour (`ConnectionNotice.strip`).
enum ConnectionStrip: Equatable {
  case down
  case connecting
}

/// Since when the connection is down, for the grace period. The device being offline gets the grace
/// period too: a network hand-over (Wi-Fi to cellular, leaving a lift) drops the path for a moment,
/// which must not flash the strip. The same rule as web, desktop and Android.
struct ConnectionGrace: Equatable {
  /// A blip shorter than this stays invisible.
  static let period: TimeInterval = 2
  /// After the app's return the reconnect it starts gets this long (issue #713).
  static let returnPeriod: TimeInterval = 10

  private(set) var maybeDownSince: Date?
  private(set) var isAfterReturn = false

  /// Follows the connection: the period starts when it goes down and ends when it is up.
  mutating func observe(isDown: Bool, now: Date) {
    if !isDown {
      maybeDownSince = nil
      isAfterReturn = false
    } else if maybeDownSince == nil {
      maybeDownSince = now
    }
  }

  /// The app is back in the foreground: a connection that is down gets the longer period from now,
  /// the time in the background doesn't count.
  mutating func appReturned(now: Date) {
    if maybeDownSince != nil {
      maybeDownSince = now
      isAfterReturn = true
    }
  }

  /// Down for at least the grace period. A clock that went back counts as just now.
  func isPastGrace(now: Date) -> Bool {
    guard let since = maybeDownSince else { return false }
    return now.timeIntervalSince(since) >= (isAfterReturn ? Self.returnPeriod : Self.period)
  }
}

/// When the app renews the push registration (the core seals the device token again when that is
/// due). Each renewal needs the server and pauses syncing while it runs (every read of the local
/// store waits for it), so it waits for a connection, runs at most once a day, and a failed attempt
/// is retried after a pause instead of at every reconnect.
enum PushUpkeepSchedule {
  /// How often the reseal check runs; the core reseals after days (`push_reseal_after`).
  static let checkInterval: TimeInterval = 24 * 3600
  /// The pause after any attempt before the next one, also after a failure.
  static let retryPause: TimeInterval = 10 * 60

  /// Whether the registered token's reseal check is due. A clock that went back makes it due.
  static func isRefreshDue(lastCheckedAt maybeLast: Date?, now: Date) -> Bool {
    guard let last = maybeLast else { return true }
    let elapsed = now.timeIntervalSince(last)
    return elapsed < 0 || elapsed >= checkInterval
  }

  /// Whether another attempt may start now, after the last one (this process).
  static func mayAttempt(lastAttemptAt maybeLast: Date?, now: Date) -> Bool {
    guard let last = maybeLast else { return true }
    let elapsed = now.timeIntervalSince(last)
    return elapsed < 0 || elapsed >= retryPause
  }
}
