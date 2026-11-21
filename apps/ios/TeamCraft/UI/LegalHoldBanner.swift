import SwiftUI

/// Every member of a held conversation reads that it is held, with the case reference, and that
/// retention doesn't delete its messages (issue #211, docs/data-export.md §4.2). The hold comes from
/// the owner-signed policy this device verified, so the server can't show it for a conversation
/// nobody held. Desktop and web show the same notice (`LegalHoldNotice`); this only draws it.
struct LegalHoldBanner: View {
  let hold: LegalHold

  var body: some View {
    Label {
      Text("legal_hold_notice \(hold.reference)")
        .frame(maxWidth: .infinity, alignment: .leading)
    } icon: {
      Image(systemName: "lock.shield")
    }
    .font(.app(.footnote))
    .padding(.horizontal, 16)
    .padding(.vertical, 8)
    .background(.bar)
    .accessibilityElement(children: .combine)
  }
}
