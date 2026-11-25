import Observation
import SwiftUI
import UIKit

/// Pull down at the top of the chat list opens the search (#2389), instead of a pull to refresh
/// that had nothing to refresh. The list's own bounce is the physics: the field's preview rides in
/// the gap it opens, a tick says where letting go opens the search, and letting go earlier springs
/// back with the bounce.
///
/// The distance changes on every frame of the pull; only `PullSearchReveal` reads it, so the list
/// itself is not evaluated again while the finger moves.
@Observable
@MainActor
final class PullToSearch {
  /// How far past its top the list is pulled, in points.
  var distance: CGFloat = 0
  /// Past the threshold while the finger is down: letting go now opens the search.
  private(set) var isArmed = false
  @ObservationIgnored private var isInteracting = false
  @ObservationIgnored private let haptics = UIImpactFeedbackGenerator(style: .light)

  /// The field's height and its padding: pulled this far, the preview is whole.
  static let threshold: CGFloat = 60

  func update(distance newDistance: CGFloat) {
    distance = max(0, newDistance)
    guard isInteracting else { return }
    if distance >= Self.threshold, !isArmed {
      isArmed = true
      haptics.impactOccurred()
    } else if distance < Self.threshold, isArmed {
      isArmed = false
    }
  }

  func beginInteraction() {
    isInteracting = true
    haptics.prepare()
  }

  /// The finger let go: true when that opens the search.
  func endInteraction() -> Bool {
    isInteracting = false
    let opens = isArmed
    isArmed = false
    return opens
  }
}

/// The search field's preview in the gap a pull opens above the list: it fades and grows in with
/// the pull, and only fades with Reduce Motion.
struct PullSearchReveal: View {
  let pull: PullToSearch
  @Environment(\.accessibilityReduceMotion) private var reduceMotion

  var body: some View {
    let progress = min(1, pull.distance / PullToSearch.threshold)
    HStack(spacing: 6) {
      Image(systemName: "magnifyingglass")
      Text("search_chats_hint")
      Spacer(minLength: 0)
    }
    .foregroundStyle(Theme.textMuted)
    .padding(.leading, 12)
    .frame(minHeight: 44)
    .background(Color(.tertiarySystemFill), in: .capsule)
    .padding(.horizontal, 16)
    .padding(.bottom, 8)
    .scaleEffect(reduceMotion ? 1 : 0.9 + 0.1 * progress, anchor: .bottom)
    .opacity(progress)
    // Rides on the list's top edge and comes out from under the header: only the gap shows it.
    .frame(maxWidth: .infinity, minHeight: 0, maxHeight: max(0, pull.distance), alignment: .bottom)
    .clipped()
    .allowsHitTesting(false)
    // The header's search button is what VoiceOver uses; this preview is only a picture.
    .accessibilityHidden(true)
  }
}

extension View {
  /// Lets a pull past the list's top open the search (iOS 18: scroll geometry and phases). On
  /// iOS 17 the header's search button is the way in.
  @MainActor @ViewBuilder
  func pullsToSearch(_ pull: PullToSearch, isEnabled: Bool, onOpen: @escaping () -> Void) -> some View {
    if #available(iOS 18.0, *) {
      self
        .onScrollGeometryChange(for: CGFloat.self) { geometry in
          -(geometry.contentOffset.y + geometry.contentInsets.top)
        } action: { _, distance in
          if isEnabled { pull.update(distance: distance) }
        }
        .onScrollPhaseChange { _, phase in
          guard isEnabled else { return }
          if phase == .interacting {
            pull.beginInteraction()
          } else if pull.endInteraction() {
            onOpen()
          }
        }
        .overlay(alignment: .top) {
          if isEnabled { PullSearchReveal(pull: pull) }
        }
    } else {
      self
    }
  }
}
