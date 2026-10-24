import SwiftUI

/// Sign-in: the server's brand (logo, heading, text, links), then the ways the server offers in its
/// order – "Sign in with …" through the identity provider and/or e-mail and password of a local
/// account – and which server the app signs in to, so a user with several organizations knows
/// where they are. The methods are asked for each time the screen is shown.
struct SignInView: View {
  @Environment(AppModel.self) private var model
  let failure: AppFailure?

  @State private var form = LocalSignInForm()
  @State private var maybeBusyMethod: SignInOption?
  @FocusState private var maybeFocus: Field?
  @ScaledMetric(relativeTo: .headline) private var ssoIconSize: CGFloat = 22

  private enum Field: Hashable {
    case email, password, code
  }

  var body: some View {
    let brand = model.brand
    ScrollView {
      VStack(spacing: 24) {
        header(brand)
        if let failure {
          Text(failure.message).foregroundStyle(Theme.danger).multilineTextAlignment(.center)
        }
        if let page = model.maybeSignInPage {
          methods(page)
          help(page)
        } else {
          ProgressView()
            .frame(maxWidth: .infinity, minHeight: 88)
        }
        Label("sign_in_e2ee_note", systemImage: "lock.fill")
          .font(.footnote)
          .foregroundStyle(Theme.textMuted)
          .multilineTextAlignment(.center)
        footer(brand)
      }
      .padding(24)
      .frame(maxWidth: 480)
      .frame(maxWidth: .infinity)
    }
    .scrollBounceBehavior(.basedOnSize)
    .scrollDismissesKeyboard(.interactively)
    .task { await model.refreshSignInPage() }
    .task { await brand.refresh() }
    // The password is not kept around once the screen is gone.
    .onDisappear { form = LocalSignInForm() }
  }

  // MARK: Sections

  @ViewBuilder
  private func header(_ brand: BrandStore) -> some View {
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
  }

  @ViewBuilder
  private func methods(_ page: SignInPageInfo) -> some View {
    let sections = SignInSection.layout(methods: page.methods)
    // SSO is the prominent button when offered; otherwise the local form's button is.
    let isLocalProminent = !sections.contains(.sso)
    ForEach(Array(sections.enumerated()), id: \.offset) { _, section in
      switch section {
      case .sso: ssoButton(page)
      case .divider: OrDivider()
      case .local: localForm(requiresCode: page.localRequiresCode, isProminent: isLocalProminent)
      }
    }
  }

  private func ssoButton(_ page: SignInPageInfo) -> some View {
    SignInActionButton(isProminent: true, action: signInWithSso) {
      if maybeBusyMethod == .sso {
        ProgressView().tint(Theme.onAccent)
      } else {
        HStack(spacing: 10) {
          if let raw = page.maybeSsoIconUrl, let url = URL(string: raw) {
            AsyncImage(url: url) { image in
              image.resizable().scaledToFit()
            } placeholder: {
              Color.clear
            }
            .frame(width: ssoIconSize, height: ssoIconSize)
            .accessibilityHidden(true)
          }
          Text("sign_in_with \(page.ssoLabel)")
        }
      }
    }
    .disabled(model.isBusy)
  }

  private func localForm(requiresCode: Bool, isProminent: Bool) -> some View {
    VStack(spacing: 12) {
      TextField("sign_in_email", text: $form.email)
        .textContentType(.username)
        .keyboardType(.emailAddress)
        .textInputAutocapitalization(.never)
        .autocorrectionDisabled()
        .submitLabel(.next)
        .focused($maybeFocus, equals: .email)
        .onSubmit { maybeFocus = .password }
        .signInField()
      HStack(spacing: 8) {
        Group {
          if form.isPasswordVisible {
            TextField("sign_in_password", text: $form.password)
          } else {
            SecureField("sign_in_password", text: $form.password)
          }
        }
        .textContentType(.password)
        .textInputAutocapitalization(.never)
        .autocorrectionDisabled()
        .submitLabel(requiresCode ? .next : .go)
        .focused($maybeFocus, equals: .password)
        .onSubmit {
          if requiresCode {
            maybeFocus = .code
          } else {
            submitLocal(requiresCode: requiresCode)
          }
        }
        Button {
          form.isPasswordVisible.toggle()
          maybeFocus = .password
        } label: {
          Image(systemName: form.isPasswordVisible ? "eye.slash" : "eye")
            .frame(minWidth: 32, minHeight: 32)
        }
        .buttonStyle(.borderless)
        .foregroundStyle(Theme.textMuted)
        .accessibilityLabel(
          Text(
            form.isPasswordVisible
              ? LocalizedStringKey("sign_in_hide_password") : LocalizedStringKey("sign_in_show_password")))
      }
      .signInField()
      // Shown from the start: the server never says whether an account has a code (no oracle).
      TextField(
        requiresCode ? LocalizedStringKey("sign_in_code_required") : LocalizedStringKey("sign_in_code_optional"),
        text: $form.code
      )
      .textContentType(.oneTimeCode)
      .keyboardType(.numberPad)
      .font(.body.monospacedDigit())
      .focused($maybeFocus, equals: .code)
      .onChange(of: form.code) { _, newValue in
        let clean = LocalSignInForm.sanitizedCode(newValue)
        if clean != newValue { form.code = clean }
      }
      .signInField()
      if let failure = form.maybeFailure {
        Text(failure.message)
          .foregroundStyle(Theme.danger)
          .multilineTextAlignment(.center)
      }
      SignInActionButton(isProminent: isProminent, action: { submitLocal(requiresCode: requiresCode) }) {
        if maybeBusyMethod == .local {
          ProgressView().tint(isProminent ? Theme.onAccent : Theme.accent)
        } else {
          Text("sign_in")
        }
      }
      .disabled(model.isBusy || !form.canSubmit(requiresCode: requiresCode))
    }
    .disabled(model.isBusy)
  }

  @ViewBuilder
  private func help(_ page: SignInPageInfo) -> some View {
    if page.maybeHelpText != nil || page.maybeHelpLink != nil {
      VStack(spacing: 8) {
        if let text = page.maybeHelpText {
          Text(verbatim: text)
            .foregroundStyle(Theme.textMuted)
            .multilineTextAlignment(.center)
        }
        if let raw = page.maybeHelpLink, let url = URL(string: raw) {
          Link("sign_in_help_link", destination: url)
        }
      }
      .font(.footnote)
    }
  }

  @ViewBuilder
  private func footer(_ brand: BrandStore) -> some View {
    VStack(spacing: 8) {
      Text("sign_in_server \(brand.serverHost)")
        .foregroundStyle(Theme.textMuted)
      if !model.isServerLocked {
        Button("change_server") { model.changeServer() }
          .disabled(model.isBusy)
      }
    }
    .font(.footnote)
    if let links = brand.brand?.links { BrandLinkRow(links: links) }
  }

  // MARK: Actions

  private func signInWithSso() {
    guard !model.isBusy else { return }
    maybeBusyMethod = .sso
    Task {
      await model.signIn()
      maybeBusyMethod = nil
    }
  }

  private func submitLocal(requiresCode: Bool) {
    guard !model.isBusy, form.canSubmit(requiresCode: requiresCode) else { return }
    let email = form.trimmedEmail
    let password = form.password
    let maybeCode = form.maybeCode
    form.maybeFailure = nil
    maybeBusyMethod = .local
    Task {
      let maybeFailure = await model.signInLocal(email: email, password: password, maybeCode: maybeCode)
      maybeBusyMethod = nil
      guard let failure = maybeFailure else { return }
      form.apply(failure)
      if failure == .wrongCredentials { maybeFocus = .password }
    }
  }
}

/// A full-width sign-in button: filled in the accent color (the primary way), else outlined.
private struct SignInActionButton<Content: View>: View {
  let isProminent: Bool
  let action: () -> Void
  @ViewBuilder let content: () -> Content

  var body: some View {
    Group {
      if isProminent {
        Button(action: action) {
          label.foregroundStyle(Theme.onAccent)
        }
        .buttonStyle(.borderedProminent)
      } else {
        Button(action: action) { label }
          .buttonStyle(.bordered)
      }
    }
    .controlSize(.large)
    .tint(Theme.accent)
  }

  private var label: some View {
    content()
      .font(.headline)
      .frame(maxWidth: .infinity)
      .padding(.vertical, 4)
  }
}

/// "or" between two ways to sign in.
private struct OrDivider: View {
  var body: some View {
    HStack(spacing: 12) {
      line
      Text("sign_in_or")
        .font(.footnote)
        .foregroundStyle(Theme.textMuted)
      line
    }
  }

  private var line: some View {
    Rectangle()
      .fill(Theme.border)
      .frame(height: 1)
      .accessibilityHidden(true)
  }
}

extension View {
  /// A text field on the sign-in screen: padded, on the surface color, with a visible edge.
  fileprivate func signInField() -> some View {
    padding(12)
      .background(Theme.surface)
      .clipShape(RoundedRectangle(cornerRadius: Theme.radius))
      .overlay(RoundedRectangle(cornerRadius: Theme.radius).stroke(Theme.controlBorder))
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
