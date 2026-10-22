import SwiftUI

struct SignInView: View {
  @Environment(AppModel.self) private var model
  let failure: AppFailure?

  var body: some View {
    VStack(spacing: 24) {
      Spacer()
      Image(systemName: "bubble.left.and.bubble.right.fill")
        .font(.system(size: 56))
        .foregroundStyle(Theme.accent)
      Text("app_name").font(.largeTitle.bold())
      Text("sign_in_hint")
        .multilineTextAlignment(.center)
        .foregroundStyle(Theme.textMuted)
      if let failure {
        Text(failure.message).foregroundStyle(Theme.danger).multilineTextAlignment(.center)
      }
      Spacer()
      Button("sign_in") { Task { await model.signIn() } }
        .buttonStyle(PrimaryButtonStyle())
        .disabled(model.isBusy)
    }
    .padding(24)
  }
}
