import SwiftUI

/// The search filters (from, in, date range, files, links) as a form; the search screen writes
/// them into the core's query syntax (`SearchFilters`).
struct SearchFiltersView: View {
  @Environment(\.dismiss) private var dismiss
  @Binding var filters: SearchFilters

  var body: some View {
    NavigationStack {
      Form {
        Section("search_filter_who_where") {
          TextField("search_filter_from", text: $filters.from)
            .textInputAutocapitalization(.never).autocorrectionDisabled()
          TextField("search_filter_in", text: $filters.channel)
            .textInputAutocapitalization(.never).autocorrectionDisabled()
        }
        Section("search_filter_when") {
          optionalDate("search_filter_after", date: $filters.after)
          optionalDate("search_filter_before", date: $filters.before)
        }
        Section("search_filter_what") {
          Toggle("search_filter_has_file", isOn: $filters.hasFile)
          Toggle("search_filter_has_link", isOn: $filters.hasLink)
        }
        Section {
          Button("search_filter_clear", role: .destructive) { filters = SearchFilters() }
            .disabled(filters.isEmpty)
        }
      }
      .navigationTitle("search_filters_title")
      .navigationBarTitleDisplayMode(.inline)
      .toolbar {
        ToolbarItem(placement: .confirmationAction) { Button("done") { dismiss() } }
      }
    }
  }

  /// A date that can be switched off.
  private func optionalDate(_ title: LocalizedStringKey, date: Binding<Date?>) -> some View {
    HStack {
      Toggle(
        title,
        isOn: Binding(
          get: { date.wrappedValue != nil },
          set: { date.wrappedValue = $0 ? (date.wrappedValue ?? Date()) : nil }))
      if let value = date.wrappedValue {
        DatePicker(
          "", selection: Binding(get: { value }, set: { date.wrappedValue = $0 }), displayedComponents: .date
        )
        .labelsHidden()
      }
    }
  }
}
