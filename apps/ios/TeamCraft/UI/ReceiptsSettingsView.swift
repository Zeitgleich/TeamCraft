import SwiftUI

/// Whether others see the user's read and delivered ticks (docs/notifications.md, "Read
/// receipts"). Stored on the server, reciprocal: turning one off also hides the others' from the
/// user.
struct ReceiptsSettingsView: View {
  @Environment(AppModel.self) private var model

  @State private var sendsRead = true
  @State private var sendsDelivered = true
  @State private var isLoaded = false
  /// What the server has, so opening the screen doesn't turn the defaults into a choice.
  @State private var saved = (read: true, delivered: true)

  var body: some View {
    Form {
      Section {
        Toggle("receipts_read", isOn: $sendsRead)
        Toggle("receipts_delivered", isOn: $sendsDelivered)
      } footer: {
        Text("receipts_hint")
      }
    }
    .navigationTitle("receipts_title")
    .navigationBarTitleDisplayMode(.inline)
    .disabled(!isLoaded)
    .task { await load() }
    .onChange(of: sendsRead) { _, _ in Task { await save() } }
    .onChange(of: sendsDelivered) { _, _ in Task { await save() } }
  }

  private func load() async {
    guard let json = await model.notificationSettingsJSON() else { return }
    let current = NotificationSettingsDocument.receipts(in: json)
    saved = current
    sendsRead = current.read
    sendsDelivered = current.delivered
    isLoaded = true
  }

  private func save() async {
    guard isLoaded, (sendsRead, sendsDelivered) != saved else { return }
    saved = (sendsRead, sendsDelivered)
    await model.saveReceipts(read: sendsRead, delivered: sendsDelivered)
  }
}
