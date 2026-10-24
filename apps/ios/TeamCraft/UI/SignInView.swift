import SwiftUI

/// Sign-in: the server's brand (logo, heading, text, links) when it sent one, and which server the
/// app signs in to, so a user with several organizations knows where they are.
struct SignInView: View {
  @Environment(AppModel.self) private var model
  let failure: AppFailure?

  var body: some View {
    let brand = model.brand
    ScrollView {
      VStack(spacing: 24) {
        BrandLogo(brand: brand)
        Group {
          if let title = brand.brand?.maybeTitle ?? brand.brand?.displayName {
            Text(verbatim: title)
          } else {
            Text("app_name")
          }
        }
        .font(.largeTitle.bold())
        .multilineTextAlignment(.center)
        .accessibilityAddTraits(.isHeader)
        Group {
          if let text = brand.brand?.maybeText { Text(verbatim: text) } else { Text("sign_in_hint") }
        }
        .multilineTextAlignment(.center)
        .foregroundStyle(Theme.textMuted)
        if let failure {
          Text(failure.message).foregroundStyle(Theme.danger).multilineTextAlignment(.center)
        }
        Button {
          Task { await model.signIn() }
        } label: {
          if model.isBusy { ProgressView() } else { Text("sign_in") }
        }
        .buttonStyle(PrimaryButtonStyle())
        .disabled(model.isBusy)
        Text("sign_in_server \(brand.serverHost)")
          .font(.footnote)
          .foregroundStyle(Theme.textMuted)
        if !model.isServerLocked {
          Button("change_server") { model.changeServer() }
            .font(.footnote)
            .disabled(model.isBusy)
        }
        if let links = brand.brand?.links { BrandLinkRow(links: links) }
      }
      .padding(24)
      .frame(maxWidth: 480)
      .frame(maxWidth: .infinity)
    }
    .scrollBounceBehavior(.basedOnSize)
    .task { await brand.refresh() }
  }
}

/// Support, privacy, terms and imprint, as the brand names them; each opens in the browser.
private struct BrandLinkRow: View {
  let links: BrandLinks

  var body: some View {
    let entries: [(LocalizedStringKey, String?)] = [
      ("brand_support", links.maybeSupport), ("brand_privacy", links.maybePrivacy),
      ("brand_terms", links.maybeTerms), ("brand_imprint", links.maybeImprint),
    ]
    ViewThatFits {
      HStack(spacing: 16) { linkButtons(entries) }
      VStack(spacing: 8) { linkButtons(entries) }
    }
    .font(.footnote)
  }

  @ViewBuilder
  private func linkButtons(_ entries: [(LocalizedStringKey, String?)]) -> some View {
    ForEach(Array(entries.enumerated()), id: \.offset) { _, entry in
      if let raw = entry.1, let url = URL(string: raw) {
        Link(entry.0, destination: url)
      }
    }
  }
}
