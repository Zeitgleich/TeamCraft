import SafariServices
import SwiftUI

/// The address of a link in the browser. Not in this app: Safari's own view is what iOS doesn't send
/// back to the app when the host is associated with it.
struct BrowserLink: Identifiable {
  let url: URL
  var id: String { url.absoluteString }
}

/// The system's browser view for a link that opens on the web.
struct SafariView: UIViewControllerRepresentable {
  let url: URL

  func makeUIViewController(context: Context) -> SFSafariViewController {
    SFSafariViewController(url: url)
  }

  func updateUIViewController(_ controller: SFSafariViewController, context: Context) {}
}

extension View {
  /// The question a link asks before anything happens: never opened silently, always naming the
  /// host, and the browser view it may lead to (docs/app-links.md).
  func appLinkQuestions(_ model: AppModel) -> some View {
    modifier(AppLinkQuestions(model: model))
  }
}

private struct AppLinkQuestions: ViewModifier {
  @Bindable var model: AppModel

  func body(content: Content) -> some View {
    content
      // The end of the host that says who owns it is the title; the message has the whole host, which
      // an alert wraps, so a long name can't hide it.
      .alert(Text(verbatim: questionDomain), isPresented: isAskingHost, presenting: model.appLinkQuestion) { question in
        if case .askForeignServer(let host, _, true) = question.step {
          Button("app_link_sign_in") { model.signInForAppLink(host: host) }
        }
        Button("app_link_open_browser") { model.openAppLinkInBrowser() }
        Button("cancel", role: .cancel) { model.dropAppLink() }
      } message: { question in
        Text(questionText(question.step))
      }
      .alert("app_link_meeting_title", isPresented: $model.isAskingAboutMeetingLink) {
        Button("app_link_my_meetings") { model.showMeetingsForAppLink() }
        Button("app_link_open_browser") { model.openAppLinkInBrowser() }
        Button("cancel", role: .cancel) { model.dropAppLink() }
      } message: {
        Text("app_link_meeting")
      }
      .alert("permalink_unavailable", isPresented: $model.isPermalinkFromLinkMissing) {
        Button("done", role: .cancel) {}
      }
      .sheet(item: $model.browserLink) { link in SafariView(url: link.url) }
  }

  private var isAskingHost: Binding<Bool> {
    Binding(get: { model.appLinkQuestion != nil }, set: { _ in })
  }

  private var questionDomain: String {
    switch model.appLinkQuestion?.step {
    case .askForeignServer(_, let domain, _), .askWebOnly(_, let domain): domain
    default: ""
    }
  }

  private func questionText(_ step: OpenedAppLinkStep) -> LocalizedStringKey {
    switch step {
    case .askForeignServer(let host, _, true): "app_link_foreign_add \(host)"
    case .askForeignServer(let host, _, false): "app_link_foreign_other \(host)"
    case .askWebOnly(let host, _): "app_link_web_only \(host)"
    case .navigate: ""
    }
  }
}
