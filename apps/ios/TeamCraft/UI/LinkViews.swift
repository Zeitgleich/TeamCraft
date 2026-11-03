import AVFoundation
import SwiftUI
import VisionKit

/// Existing device: the user types the code the new device shows. The core checks it (three wrong
/// codes end the attempt); the code is never shown here (docs/e2ee.md §5). Used by the prompt for a
/// new device and by Settings → Devices.
struct LinkTypedCodeForm: View {
  @Environment(AppModel.self) private var model

  let onApproved: () -> Void
  /// The last try failed; the attempt is over and the new device has to start again.
  let onEnded: () -> Void
  let onCancel: () -> Void

  @State private var typed = ""
  /// A missing or short code, or the other device's refusal; shown at the field.
  @State private var maybeProblem: TextProblem?
  @State private var isWorking = false
  @FocusState private var isFocused: Bool
  @AccessibilityFocusState private var isAccessibilityFocused: Bool

  var body: some View {
    VStack(alignment: .leading, spacing: 8) {
      Text("link_type_code_generic")
      TextField("link_typed_code", text: $typed)
        .keyboardType(.numberPad)
        .autocorrectionDisabled()
        .font(.system(.title2, design: .monospaced))
        .padding(8)
        .background(Theme.surface)
        .clipShape(RoundedRectangle(cornerRadius: 8))
        .fieldEdge(isError: maybeProblem != nil, cornerRadius: 8)
        .focused($isFocused)
        .accessibilityFocused($isAccessibilityFocused)
        .fieldError(maybeProblem?.message)
        .onSubmit { Task { await approve() } }
        // Errors follow the input: typing takes the message away.
        .onChange(of: typed) { _, newValue in
          if !newValue.isEmpty { maybeProblem = nil }
        }
      FieldMessage(maybeError: maybeProblem?.message, maybeHint: "link_code_digits")
      HStack {
        Spacer()
        Button("cancel", action: onCancel)
          .buttonStyle(.bordered)
        Button("link_approve") { Task { await approve() } }
          .buttonStyle(.borderedProminent)
          .tint(Theme.accent)
          .disabled(isWorking)
      }
    }
  }

  private func approve() async {
    guard !isWorking else { return }
    if let problem = LinkCode.problem(typed: typed) {
      showAtField(problem)
      return
    }
    isWorking = true
    maybeProblem = nil
    switch await model.confirmLink(typedCode: typed) {
    case .approved: onApproved()
    case .wrongCode:
      typed = ""
      showAtField(.rejected("link_wrong_code"))
    case .attemptEnded: onEnded()
    case .failed(let failure): showAtField(.refused(failure))
    }
    isWorking = false
  }

  private func showAtField(_ problem: TextProblem) {
    maybeProblem = problem
    isFocused = true
    isAccessibilityFocused = true
  }
}

/// A new device of the user asks to be linked: a card at the top of the signed-in UI on every
/// device. It is not modal and takes no focus. It never approves by itself and never shows the
/// code; Review leads to the typed-code step (docs/e2ee.md §5, "Prompt on every device").
struct LinkPromptOverlay: View {
  @Environment(AppModel.self) private var model

  private enum Notice: Equatable {
    case approved, ended
  }

  @State private var maybeNotice: Notice?

  var body: some View {
    VStack(spacing: 8) {
      if let notice = maybeNotice {
        noticeCard(notice)
      }
      if let review = model.maybeLinkReview, review.maybeRequestId != nil {
        card {
          LinkTypedCodeForm(
            onApproved: { maybeNotice = .approved },
            onEnded: { maybeNotice = .ended },
            onCancel: { Task { await model.cancelLinkApproval() } })
        }
      } else if let prompt = model.linkPrompts.first {
        LinkRequestCard(prompt: prompt).id(prompt.id)
      }
    }
    .padding(.horizontal)
    .padding(.top, 4)
    .animation(.default, value: model.linkPrompts.map(\.id))
    .task(id: maybeNotice) {
      // The success notice is brief; the failure stays until it is dismissed.
      guard maybeNotice == .approved else { return }
      try? await Task.sleep(for: .seconds(4))
      if !Task.isCancelled { maybeNotice = nil }
    }
  }

  private func noticeCard(_ notice: Notice) -> some View {
    card {
      switch notice {
      case .approved:
        Label("approve_done", systemImage: "checkmark.circle").foregroundStyle(Theme.text)
      case .ended:
        VStack(alignment: .leading, spacing: 8) {
          Label("link_attempt_ended", systemImage: "exclamationmark.triangle").foregroundStyle(Theme.danger)
          HStack {
            Spacer()
            Button("done") { maybeNotice = nil }.buttonStyle(.bordered)
          }
        }
      }
    }
  }

  private func card<Content: View>(@ViewBuilder _ content: () -> Content) -> some View {
    LinkCardChrome(content: content())
  }
}

/// The first open request: who asks, a warning, Deny and Review.
private struct LinkRequestCard: View {
  @Environment(AppModel.self) private var model
  let prompt: LinkPrompt

  @State private var maybeError: LocalizedStringKey?
  @State private var isWorking = false

  var body: some View {
    LinkCardChrome(
      content: VStack(alignment: .leading, spacing: 8) {
        Label("link_prompt_title", systemImage: "iphone.and.arrow.forward")
          .font(.headline)
          .foregroundStyle(Theme.text)
        VStack(alignment: .leading, spacing: 2) {
          // Both are the new device's own claim, nothing checked them.
          Text("link_prompt_says \(prompt.deviceName) \(prompt.platform.label)").font(.subheadline.weight(.semibold))
          Text("link_prompt_unverified").font(.footnote).foregroundStyle(Theme.textMuted)
        }
        Text("link_prompt_warning").font(.footnote).foregroundStyle(Theme.textMuted)
        if let error = maybeError {
          Text(error).font(.footnote).foregroundStyle(Theme.danger)
        }
        HStack {
          Spacer()
          Button("link_deny", role: .destructive) { Task { await deny() } }
            .buttonStyle(.bordered)
          Button("link_review") { Task { await review() } }
            .buttonStyle(.borderedProminent)
            .tint(Theme.accent)
        }
        .disabled(isWorking)
      })
  }

  private func deny() async {
    await run { try await model.rejectLinkRequest(prompt) }
  }

  private func review() async {
    await run { try await model.reviewLinkRequest(prompt) }
  }

  private func run(_ action: () async throws -> Void) async {
    isWorking = true
    maybeError = nil
    do {
      try await action()
    } catch {
      maybeError = AppFailure(CoreFailure(error)).message
    }
    isWorking = false
  }
}

private struct LinkCardChrome<Content: View>: View {
  let content: Content

  var body: some View {
    content
      .frame(maxWidth: .infinity, alignment: .leading)
      .padding()
      .background(Theme.surface)
      .clipShape(RoundedRectangle(cornerRadius: Theme.radius))
      .overlay(RoundedRectangle(cornerRadius: Theme.radius).stroke(Theme.border))
      .shadow(color: .black.opacity(0.15), radius: 8, y: 2)
      .accessibilityElement(children: .contain)
  }
}

// MARK: Scanning the code

/// Camera access for the scanner, as the status the user can act on.
enum LinkScanAccess: Equatable {
  case granted, denied, unavailable

  /// Asks for the camera (the system prompt shows once), then checks that the scanner runs here.
  /// VisionKit's `DataScannerViewController` needs an A12 chip or newer, which every device that runs
  /// iOS 17 has; the check still covers a missing or blocked camera.
  @MainActor
  static func request() async -> LinkScanAccess {
    switch AVCaptureDevice.authorizationStatus(for: .video) {
    case .authorized: break
    case .notDetermined:
      guard await AVCaptureDevice.requestAccess(for: .video) else { return .denied }
    default: return .denied
    }
    return DataScannerViewController.isSupported && DataScannerViewController.isAvailable ? .granted : .unavailable
  }
}

/// Scans the QR code another device shows and hands its text over. Without camera access or a
/// scanner it says so and the user types the code instead.
struct ScanLinkCodeView: View {
  @Environment(\.dismiss) private var dismiss
  let onCode: (String) -> Void

  @State private var maybeAccess: LinkScanAccess?
  @State private var hasScannerFailed = false

  var body: some View {
    NavigationStack {
      Group {
        switch maybeAccess {
        case nil:
          ProgressView()
        case .granted where !hasScannerFailed:
          QRScannerRepresentable(onCode: onCode, onFailure: { hasScannerFailed = true })
            .ignoresSafeArea()
            .overlay(alignment: .bottom) {
              Text("scan_hint")
                .font(.subheadline)
                .padding(10)
                .background(.regularMaterial)
                .clipShape(RoundedRectangle(cornerRadius: Theme.radius))
                .padding()
            }
        case .denied:
          message("scan_denied", showsSettings: true)
        case .granted, .unavailable:
          message("scan_unavailable", showsSettings: false)
        }
      }
      .navigationTitle("scan_title")
      .navigationBarTitleDisplayMode(.inline)
      .toolbar {
        ToolbarItem(placement: .cancellationAction) {
          Button("cancel") { dismiss() }
        }
      }
    }
    .task { maybeAccess = await LinkScanAccess.request() }
  }

  private func message(_ key: LocalizedStringKey, showsSettings: Bool) -> some View {
    VStack(spacing: Theme.spacing) {
      Text(key).multilineTextAlignment(.center).foregroundStyle(Theme.textMuted)
      if showsSettings {
        Button("scan_open_settings") {
          if let url = URL(string: UIApplication.openSettingsURLString) { UIApplication.shared.open(url) }
        }
      }
      Button("scan_type_instead") { dismiss() }
    }
    .padding(24)
  }
}

/// VisionKit's scanner, QR codes only. It reports the first code once and stops.
private struct QRScannerRepresentable: UIViewControllerRepresentable {
  let onCode: (String) -> Void
  let onFailure: () -> Void

  func makeCoordinator() -> Coordinator {
    Coordinator(onCode: onCode)
  }

  func makeUIViewController(context: Context) -> DataScannerViewController {
    let scanner = DataScannerViewController(
      recognizedDataTypes: [.barcode(symbologies: [.qr])],
      qualityLevel: .balanced,
      recognizesMultipleItems: false,
      isHighFrameRateTrackingEnabled: false,
      isPinchToZoomEnabled: true,
      isGuidanceEnabled: false,
      isHighlightingEnabled: true)
    scanner.delegate = context.coordinator
    return scanner
  }

  func updateUIViewController(_ scanner: DataScannerViewController, context: Context) {
    guard !context.coordinator.hasStarted else { return }
    context.coordinator.hasStarted = true
    do {
      try scanner.startScanning()
    } catch {
      // The state of the parent view must not change during this update.
      let onFailure = onFailure
      Task { @MainActor in onFailure() }
    }
  }

  static func dismantleUIViewController(_ scanner: DataScannerViewController, coordinator: Coordinator) {
    scanner.stopScanning()
  }

  @MainActor
  final class Coordinator: NSObject, DataScannerViewControllerDelegate {
    let onCode: (String) -> Void
    var hasStarted = false
    private var hasReported = false

    init(onCode: @escaping (String) -> Void) {
      self.onCode = onCode
    }

    func dataScanner(
      _ dataScanner: DataScannerViewController, didAdd addedItems: [RecognizedItem], allItems: [RecognizedItem]
    ) {
      report(addedItems, from: dataScanner)
    }

    func dataScanner(_ dataScanner: DataScannerViewController, didTapOn item: RecognizedItem) {
      report([item], from: dataScanner)
    }

    private func report(_ items: [RecognizedItem], from scanner: DataScannerViewController) {
      guard !hasReported else { return }
      for case .barcode(let barcode) in items {
        guard let payload = barcode.payloadStringValue, !payload.isEmpty else { continue }
        hasReported = true
        scanner.stopScanning()
        UINotificationFeedbackGenerator().notificationOccurred(.success)
        onCode(payload)
        return
      }
    }
  }
}
