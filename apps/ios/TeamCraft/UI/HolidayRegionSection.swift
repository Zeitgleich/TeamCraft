import SwiftUI

/// Whose public holidays the user's repeating meetings skip (#1450): the workspace's region or one
/// of their own. Saved as soon as it is picked, the picker waits while it saves, and a refusal puts
/// the old choice back and says so. Offline the row is off: it is a setting, not content.
struct HolidayRegionSection: View {
  @Environment(AppModel.self) private var model
  private let network = NetworkMonitor.shared

  @State private var maybeState: HolidayRegionState?
  @State private var hasFailedToLoad = false
  /// The choice being saved, shown until the server answers; `""` is the workspace's region.
  @State private var maybePicked: String?
  @State private var maybeProblem: TextProblem?

  var body: some View {
    Section {
      Picker("holiday_region_label", selection: selection) {
        Text("holiday_region_instance").tag("")
        ForEach(HolidayRegions.codes, id: \.self) { code in
          Text(LocalizedStringKey(HolidayRegions.titleKey(for: code))).tag(code)
        }
      }
      .disabled(!isEnabled)
      .accessibilityIdentifier("holidayRegionPicker")
      .accessibilityHint(hintText)
      if let maybeProblem {
        FieldMessage(maybeError: maybeProblem.message)
      }
    } header: {
      Text("holiday_title")
    } footer: {
      VStack(alignment: .leading, spacing: 4) {
        if !network.isOnline {
          Text("holiday_offline")
        } else if hasFailedToLoad {
          Text("holiday_unavailable")
        } else if let state = maybeState {
          if state.region != nil {
            Text("holiday_own_hint")
          } else {
            Text("holiday_instance_hint \(HolidayRegions.name(for: state.instanceRegion))")
          }
          if state.hasInstanceFeed {
            Text("holiday_feed_hint")
          }
        }
      }
    }
    .task(id: network.isOnline) { await load() }
  }

  private var isEnabled: Bool { network.isOnline && maybeState != nil && maybePicked == nil }

  private var hintText: Text {
    if !network.isOnline { return Text("holiday_offline") }
    if hasFailedToLoad { return Text("holiday_unavailable") }
    return Text(verbatim: "")
  }

  private var selection: Binding<String> {
    Binding(
      get: { maybePicked ?? maybeState?.region ?? "" },
      set: { choice in
        guard choice != (maybeState?.region ?? "") else { return }
        Task { await choose(choice) }
      })
  }

  private func load() async {
    guard network.isOnline, maybeState == nil else { return }
    let state = await model.holidayRegion()
    maybeState = state
    hasFailedToLoad = state == nil
  }

  private func choose(_ choice: String) async {
    guard maybePicked == nil else { return }
    maybePicked = choice
    maybeProblem = nil
    switch await model.setHolidayRegion(choice.isEmpty ? nil : choice) {
    case .saved(let maybeSaved):
      maybeState = maybeSaved
      if maybeSaved == nil { maybeState = await model.holidayRegion() }
    case .failed(let problem):
      // The old choice is back: the pick was only held while saving.
      maybeProblem = problem
    }
    maybePicked = nil
  }
}
