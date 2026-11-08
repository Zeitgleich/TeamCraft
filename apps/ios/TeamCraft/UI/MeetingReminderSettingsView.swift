import SwiftUI

/// When a planned meeting notifies (docs/meetings.md, "Starting"): some minutes before and at its
/// start. Stored on the server, which sends both at their time, so every device sees the same.
struct MeetingReminderSettingsView: View {
  @Environment(AppModel.self) private var model

  @State private var minutes = NotificationSettingsDocument.defaultReminderMinutes
  @State private var notifiesStart = true
  @State private var isLoaded = false
  /// What the server has, so opening the screen doesn't turn the defaults into a choice.
  @State private var saved = (minutes: NotificationSettingsDocument.defaultReminderMinutes, notifiesStart: true)

  private static let choices = [0, 1, 5, 10, 15]

  var body: some View {
    Form {
      Section {
        Picker("meeting_reminder_title", selection: $minutes) {
          ForEach(Self.choices, id: \.self) { choice in
            if choice == 0 {
              Text("meeting_reminder_off")
            } else {
              Text("meeting_reminder_minutes \(choice)")
            }
          }
        }
        Toggle("meeting_notify_start", isOn: $notifiesStart)
      } footer: {
        Text("meeting_reminder_hint")
      }
    }
    .navigationTitle("meeting_reminder_title")
    .navigationBarTitleDisplayMode(.inline)
    .disabled(!isLoaded)
    .task { await load() }
    .onChange(of: minutes) { _, _ in Task { await save() } }
    .onChange(of: notifiesStart) { _, _ in Task { await save() } }
  }

  private func load() async {
    guard let json = await model.notificationSettingsJSON() else { return }
    let current = NotificationSettingsDocument.meetingReminder(in: json)
    saved = current
    minutes = current.minutes
    notifiesStart = current.notifiesStart
    isLoaded = true
  }

  private func save() async {
    guard isLoaded, (minutes, notifiesStart) != saved else { return }
    saved = (minutes, notifiesStart)
    await model.saveMeetingReminder(minutes: minutes, notifiesStart: notifiesStart)
  }
}
