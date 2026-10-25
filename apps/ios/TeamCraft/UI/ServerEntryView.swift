import SwiftUI

/// First start (or "Change server"): the server address, typed (`team.example.com`, a URL) or
/// pasted (an invite or message link). The app checks it and shows the server for confirmation.
/// With a server locked by the device's management, the address is fixed and only retried.
struct ServerEntryView: View {
  @Environment(AppModel.self) private var model
  let failure: ServerFailure?

  @State private var address: String

  init(address: String, failure: ServerFailure?) {
    self.failure = failure
    _address = State(initialValue: address)
  }

  var body: some View {
    ScrollView {
      VStack(spacing: 24) {
        Image(systemName: "server.rack")
          .font(.system(size: 48))
          .foregroundStyle(Theme.accent)
          .accessibilityHidden(true)
        Text("server_entry_title")
          .font(.largeTitle.bold())
          .multilineTextAlignment(.center)
          .accessibilityAddTraits(.isHeader)
        Text(model.isServerLocked ? LocalizedStringKey("server_managed_hint") : LocalizedStringKey("server_entry_hint"))
          .multilineTextAlignment(.center)
          .foregroundStyle(Theme.textMuted)
        TextField("server_entry_placeholder", text: $address)
          .keyboardType(.URL)
          .textContentType(.URL)
          .textInputAutocapitalization(.never)
          .autocorrectionDisabled()
          .submitLabel(.continue)
          .onSubmit(submit)
          .disabled(model.isServerLocked || model.isBusy)
          .padding(12)
          .background(Theme.surface)
          .clipShape(RoundedRectangle(cornerRadius: Theme.radius))
          .overlay(RoundedRectangle(cornerRadius: Theme.radius).stroke(Theme.controlBorder))
        if let failure {
          Text(failure.message).foregroundStyle(Theme.danger).multilineTextAlignment(.center)
        }
        Button(action: submit) {
          if model.isBusy { ProgressView() } else { Text("server_entry_continue") }
        }
        .buttonStyle(PrimaryButtonStyle())
        .disabled(model.isBusy || address.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty)
      }
      .padding(24)
      .frame(maxWidth: 480)
      .frame(maxWidth: .infinity)
    }
    .scrollBounceBehavior(.basedOnSize)
  }

  private func submit() {
    guard !model.isBusy else { return }
    let input = address
    Task { await model.checkServer(input) }
  }
}

/// The checked server before signing in: its brand and host, so the user sees where the sign-in
/// goes before entering any password.
struct ServerConfirmView: View {
  @Environment(AppModel.self) private var model
  let checked: CheckedServer

  var body: some View {
    let brand = model.brand
    ScrollView {
      VStack(spacing: 24) {
        BrandLogo(brand: brand)
        Text(verbatim: brand.brand?.displayName ?? checked.server.displayName)
          .font(.largeTitle.bold())
          .multilineTextAlignment(.center)
          .accessibilityAddTraits(.isHeader)
        Text(verbatim: checked.server.address.host)
          .font(.headline)
          .foregroundStyle(Theme.textMuted)
        Text("server_confirm_provider \(checked.identityProviderHost)")
          .multilineTextAlignment(.center)
        if checked.isIdentityProviderElsewhere {
          // A server can copy another instance's sign-in settings; a provider outside the
          // server's domain gets an explicit warning (docs/e2ee.md).
          Label("server_confirm_provider_elsewhere \(checked.identityProviderHost)", systemImage: "exclamationmark.triangle.fill")
            .foregroundStyle(.red)
            .multilineTextAlignment(.leading)
        }
        Text("server_confirm_hint")
          .multilineTextAlignment(.center)
          .foregroundStyle(Theme.textMuted)
        Button("server_confirm_continue") { model.confirmServer() }
          .buttonStyle(PrimaryButtonStyle())
        if !model.isServerLocked {
          Button("server_use_another") { model.changeServer() }
        }
      }
      .padding(24)
      .frame(maxWidth: 480)
      .frame(maxWidth: .infinity)
    }
    .scrollBounceBehavior(.basedOnSize)
    .task { await brand.refresh() }
  }
}

/// The brand's logo (the dark variant in dark mode), else the app's symbol.
struct BrandLogo: View {
  @Environment(\.colorScheme) private var colorScheme
  let brand: BrandStore

  var body: some View {
    if let image = (colorScheme == .dark ? brand.darkLogo : nil) ?? brand.logo {
      Image(uiImage: image)
        .resizable()
        .scaledToFit()
        .frame(maxWidth: 220, maxHeight: 96)
        .accessibilityLabel(Text(verbatim: brand.brand?.displayName ?? ""))
    } else {
      StillMarkView()
        .frame(width: 72, height: 72)
        .accessibilityHidden(true)
    }
  }
}

/// A server brand on the sign-in screen: TeamCraft's tile and the brand's beside it, like two app
/// icons (issue #463, as on the web). Without an icon, the brand's initial on its accent.
struct BrandTiles: View {
  @Environment(\.colorScheme) private var colorScheme
  let brand: BrandStore

  var body: some View {
    HStack(spacing: 12) {
      tile(Theme.surface) { StillMarkView().padding(8) }
      if let icon = (colorScheme == .dark ? brand.darkIcon : nil) ?? brand.icon {
        tile(Theme.surface) { Image(uiImage: icon).resizable().scaledToFill() }
      } else {
        tile(Theme.accent) {
          Text(verbatim: String((brand.brand?.displayName ?? "").prefix(1)).uppercased())
            .font(.title.bold())
            .foregroundStyle(Theme.onAccent)
        }
      }
    }
    .accessibilityHidden(true)
  }

  private func tile(_ background: Color, @ViewBuilder content: () -> some View) -> some View {
    content()
      .frame(width: 64, height: 64)
      .background(background)
      .clipShape(RoundedRectangle(cornerRadius: 16, style: .continuous))
      .overlay(RoundedRectangle(cornerRadius: 16, style: .continuous).stroke(Theme.border))
      .shadow(color: .black.opacity(0.1), radius: 8, y: 4)
  }
}
