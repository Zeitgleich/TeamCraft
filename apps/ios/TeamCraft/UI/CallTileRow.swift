import SwiftUI

/// A call in the conversation (docs/notifications.md, "Calls in the timeline"): a tile at its start
/// with who started it, the time, and "Ongoing" with Join while it runs, or its duration once over.
/// A channel's call is a huddle, as desktop and web name it.
struct CallTileRow: View {
  let call: CallTile
  let starterName: String
  let sentDate: Date
  let isChannel: Bool
  let onJoin: () -> Void

  var body: some View {
    HStack(spacing: 12) {
      Image(systemName: isChannel ? "video.fill" : "phone.fill")
        .font(.app(.subheadline))
        .foregroundStyle(iconColor)
        .frame(width: 32, height: 32)
        .background(isOngoing ? Theme.accentSoft : Theme.otherBubble, in: Circle())
        .accessibilityHidden(true)
      VStack(alignment: .leading, spacing: 2) {
        Text(title)
          .font(.app(.subheadline).weight(.semibold))
          .foregroundStyle(call.isMissed ? Theme.danger : Theme.text)
        Text(verbatim: detail)
          .font(.app(.footnote).monospacedDigit())
          .foregroundStyle(Theme.textMuted)
          .lineLimit(1)
      }
      Spacer(minLength: 0)
      if isOngoing {
        Button("call_tile_join", action: onJoin)
          .buttonStyle(.borderedProminent)
          .controlSize(.small)
      }
      Text(sentDate, style: .time)
        .font(.app(.caption2).monospacedDigit())
        .foregroundStyle(Theme.textMuted)
    }
    .padding(.horizontal, 12)
    .padding(.vertical, 10)
    .frame(maxWidth: 420, alignment: .leading)
    .overlay(RoundedRectangle(cornerRadius: Theme.radius).strokeBorder(Theme.border))
    .frame(maxWidth: .infinity, alignment: .leading)
    .padding(.horizontal, 16)
    .padding(.vertical, 4)
    .accessibilityElement(children: .combine)
  }

  private var isOngoing: Bool { call.state == .ongoing }

  private var iconColor: Color {
    if call.isMissed { return Theme.danger }
    return isOngoing ? Theme.accentStrong : Theme.textMuted
  }

  private var title: String {
    if call.isMissed { return String(localized: "call_tile_missed") }
    return isChannel ? String(localized: "call_tile_huddle") : String(localized: "call_tile_call")
  }

  private var detail: String {
    let state: String =
      switch call.state {
      case .ongoing: String(localized: "call_tile_ongoing")
      case .ended(let maybeDurationMs?): callDuration(maybeDurationMs)
      case .ended(nil): String(localized: "call_tile_ended")
      }
    return starterName.isEmpty ? state : "\(starterName) · \(state)"
  }
}

/// "3:05" or "1:02:03" for a call of that length, like desktop, web and Android.
func callDuration(_ durationMs: Int64) -> String {
  let seconds = max(durationMs, 0) / 1000
  let (hours, minutes, rest) = (seconds / 3600, seconds % 3600 / 60, seconds % 60)
  return hours > 0
    ? String(format: "%d:%02d:%02d", hours, minutes, rest)
    : String(format: "%d:%02d", minutes, rest)
}
