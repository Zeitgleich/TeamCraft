import SwiftUI
import UIKit

/// "Export diagnostics" in settings (issue #1094): the app and OS version and the core's recent
/// log lines go to the share sheet as a text file. The user picks the target; nothing leaves the
/// device before that tap.
struct DiagnosticsExportRow: View {
  @State private var maybeFile: SharedFile?
  @State private var isFailed = false

  var body: some View {
    Button {
      export()
    } label: {
      Label {
        VStack(alignment: .leading, spacing: 2) {
          Text("export_diagnostics_title")
          Text("export_diagnostics_summary").font(.caption).foregroundStyle(Theme.textMuted)
        }
      } icon: {
        Image(systemName: "square.and.arrow.up")
      }
    }
    .foregroundStyle(.primary)
    .sheet(item: $maybeFile) { file in
      ActivityView(url: file.url)
        .presentationDetents([.medium, .large])
    }
    .alert("export_diagnostics_failed", isPresented: $isFailed) {
      Button("done", role: .cancel) {}
    }
  }

  private func export() {
    do {
      maybeFile = SharedFile(url: try DiagnosticsExport.writeFile())
    } catch {
      isFailed = true
    }
  }
}

/// Builds the exported text and writes it to the temporary directory.
enum DiagnosticsExport {
  static let fileName = "teamcraft-diagnostics.txt"

  /// The OS line of the export, e.g. `iOS 18.1 (iPhone15,2)`.
  static func osVersion(
    systemName: String = UIDevice.current.systemName,
    systemVersion: String = UIDevice.current.systemVersion,
    model: String = UIDevice.current.model
  ) -> String {
    "\(systemName) \(systemVersion) (\(model))"
  }

  static var appVersion: String {
    let info = Bundle.main.infoDictionary
    let version = info?["CFBundleShortVersionString"] as? String ?? "unknown"
    let build = info?["CFBundleVersion"] as? String ?? "?"
    return "\(version) (\(build))"
  }

  static func writeFile() throws -> URL {
    let url = FileManager.default.temporaryDirectory.appendingPathComponent(fileName)
    let text = diagnosticsText(appVersion: appVersion, osVersion: osVersion())
    try text.write(to: url, atomically: true, encoding: .utf8)
    return url
  }
}

private struct SharedFile: Identifiable {
  let url: URL
  var id: URL { url }
}

/// The system share sheet for one file.
private struct ActivityView: UIViewControllerRepresentable {
  let url: URL

  func makeUIViewController(context: Context) -> UIActivityViewController {
    UIActivityViewController(activityItems: [url], applicationActivities: nil)
  }

  func updateUIViewController(_ controller: UIActivityViewController, context: Context) {}
}
