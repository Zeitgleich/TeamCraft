import SwiftUI

/// The presence dot, the same in every list and header (#1578) and in the colours and shapes of the
/// desktop and web app: busy and do not disturb share red and only do not disturb carries a bar,
/// be right back is the away colour with clock hands, offline is a grey ring. No emoji (design.md).
struct PresenceDot: View {
  let presence: Presence
  var size: CGFloat = 10

  var body: some View {
    ZStack {
      switch presence {
      case .offline:
        Circle().strokeBorder(Presence.offlineColor, lineWidth: max(1.5, size * 0.2))
      default:
        Circle().fill(presence.color)
      }
      if presence == .doNotDisturb {
        Capsule().fill(.white).frame(width: size * 0.6, height: max(1.5, size * 0.18))
      }
      if presence == .beRightBack {
        ClockHands().stroke(.white, style: StrokeStyle(lineWidth: max(1.2, size * 0.15), lineCap: .round))
          .frame(width: size, height: size)
      }
    }
    .frame(width: size, height: size)
    .accessibilityHidden(true)
  }
}

/// Two hands from the centre: up and to the right, as a clock face's 3 o'clock.
private struct ClockHands: Shape {
  func path(in rect: CGRect) -> Path {
    var path = Path()
    let center = CGPoint(x: rect.midX, y: rect.midY)
    path.move(to: center)
    path.addLine(to: CGPoint(x: rect.midX, y: rect.midY - rect.height * 0.3))
    path.move(to: center)
    path.addLine(to: CGPoint(x: rect.midX + rect.width * 0.25, y: rect.midY))
    return path
  }
}

extension Presence {
  /// The status list's order on every platform: available, busy, do not disturb, be right back,
  /// away, offline.
  static let pickerOrder: [Presence] = [.online, .busy, .doNotDisturb, .beRightBack, .away, .offline]

  static let offlineColor = Color.gray

  /// The presence dot's fill.
  var color: Color {
    switch self {
    case .online: return Theme.presence
    case .busy, .doNotDisturb: return .red
    case .beRightBack, .away: return .orange
    case .offline: return Self.offlineColor
    }
  }

  /// What others see: "Online", "Busy", …
  var label: LocalizedStringKey {
    switch self {
    case .online: return "presence_online"
    case .busy: return "presence_busy"
    case .doNotDisturb: return "presence_dnd"
    case .beRightBack: return "presence_brb"
    case .away: return "presence_away"
    case .offline: return "presence_peer_offline"
    }
  }

  /// The status list's wording: what the user does ("Available", "Appear away", "Appear offline").
  var pickTitle: LocalizedStringKey {
    switch self {
    case .online: return "status_pick_online"
    case .busy: return "status_pick_busy"
    case .doNotDisturb: return "status_pick_dnd"
    case .beRightBack: return "status_pick_brb"
    case .away: return "status_pick_away"
    case .offline: return "status_pick_offline"
    }
  }

  /// The directory's presence (`online`, `busy`, `dnd`, …).
  init(directoryWire wire: String) {
    switch wire {
    case "online": self = .online
    case "busy": self = .busy
    case "dnd": self = .doNotDisturb
    case "brb": self = .beRightBack
    case "away": self = .away
    default: self = .offline
    }
  }
}

/// "Reset after" of a manual status (#1578); `.untilTime` takes the time the user picks.
enum ResetAfter: String, CaseIterable, Identifiable {
  case thirtyMinutes, oneHour, twoHours, today, thisWeek, untilTime, never

  var id: String { rawValue }

  var title: LocalizedStringKey {
    switch self {
    case .thirtyMinutes: return "status_duration_30m"
    case .oneHour: return "status_duration_1h"
    case .twoHours: return "status_duration_2h"
    case .today: return "status_duration_today"
    case .thisWeek: return "status_duration_week"
    case .untilTime: return "status_duration_until"
    case .never: return "status_duration_never"
    }
  }

  /// When a status set at `now` ends; nil for never and for a time the user picks.
  func end(from now: Date, calendar: Calendar = .current) -> Date? {
    switch self {
    case .thirtyMinutes: return now.addingTimeInterval(30 * 60)
    case .oneHour: return now.addingTimeInterval(60 * 60)
    case .twoHours: return now.addingTimeInterval(2 * 60 * 60)
    case .today: return calendar.date(byAdding: .day, value: 1, to: calendar.startOfDay(for: now))
    case .thisWeek: return calendar.dateInterval(of: .weekOfYear, for: now)?.end
    case .untilTime, .never: return nil
    }
  }
}
