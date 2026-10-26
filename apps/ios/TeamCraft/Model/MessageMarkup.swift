import SwiftUI

/// How message permalinks start (the core's format, teamcraft-client-api `markdown`).
let permalinkPrefix = "teamcraft:/c/"

/// The colours of a body's runs, which differ inside an own (accent) bubble and another's.
struct MarkupColors: Equatable {
  let link: Color
  let mention: Color
  let mentionMe: Color
  let mentionMeBackground: Color
  let codeBackground: Color
}

/// Who can be mentioned (lower-case usernames), to tell `@alex` from an e-mail-like word, and the
/// user's own username.
struct MentionNames: Equatable {
  var usernames: Set<String> = []
  var myUsername = ""
}

/// The core's names for "everyone here" (teamcraft-client-api `notify::CHANNEL_MENTIONS`).
private let everyoneMentions: Set<String> = ["channel", "here", "all"]

/// Lines of the core's markup (the Markdown subset desktop and web render) as styled text, one
/// line break between them: bold, italic, underline and strike, inline code in monospace on a
/// tinted background without its backticks, links tappable with long URLs shortened, and mentions
/// of known people highlighted. `permalinkLabel` stands in for a bare link to a message.
func markupText(
  _ lines: [MarkupLine], colors: MarkupColors, names: MentionNames, permalinkLabel: String
) -> AttributedString {
  var text = AttributedString()
  for (index, line) in lines.enumerated() {
    if index > 0 { text.append(AttributedString("\n")) }
    for run in line.runs {
      text.append(styledRun(run, colors: colors, names: names, permalinkLabel: permalinkLabel))
    }
  }
  return text
}

private func styledRun(
  _ run: MarkupRun, colors: MarkupColors, names: MentionNames, permalinkLabel: String
) -> AttributedString {
  var piece: AttributedString
  switch run.kind {
  case .text:
    piece = AttributedString(run.text)
  case .code:
    // A space each side gives the code's background room.
    piece = AttributedString(" \(run.text) ")
    piece.font = Font.body.monospaced()
    piece.backgroundColor = colors.codeBackground
  case .link(let url, let isLabeled):
    piece = link(url: url, isLabeled: isLabeled, text: run.text, colors: colors, permalinkLabel: permalinkLabel)
  case .mention(let name):
    piece = AttributedString(run.text)
    let lowered = name.lowercased()
    let isMe = everyoneMentions.contains(lowered) || (!names.myUsername.isEmpty && lowered == names.myUsername.lowercased())
    if isMe {
      piece.inlinePresentationIntent = .stronglyEmphasized
      piece.foregroundColor = colors.mentionMe
      piece.backgroundColor = colors.mentionMeBackground
    } else if names.usernames.contains(lowered) {
      piece.inlinePresentationIntent = .stronglyEmphasized
      piece.foregroundColor = colors.mention
    }
  }
  var intent = piece.inlinePresentationIntent ?? []
  if run.isBold { intent.insert(.stronglyEmphasized) }
  if run.isItalic { intent.insert(.emphasized) }
  if !intent.isEmpty { piece.inlinePresentationIntent = intent }
  if run.isStrike { piece.strikethroughStyle = Text.LineStyle.single }
  if run.isUnderline { piece.underlineStyle = Text.LineStyle.single }
  return piece
}

/// A link the shared rule opens (web links) or a message permalink, which the conversation opens
/// in the app; anything else stays the text as written. A labeled link keeps its label, a bare one
/// shows shortened.
private func link(
  url: String, isLabeled: Bool, text: String, colors: MarkupColors, permalinkLabel: String
) -> AttributedString {
  let isPermalink = url.hasPrefix(permalinkPrefix)
  guard isPermalink || isOpenableLink(url: url), let target = URL(string: url) else {
    return AttributedString(text)
  }
  let label = isLabeled ? text : (isPermalink ? permalinkLabel : shortLinkLabel(url))
  var piece = AttributedString(label)
  piece.link = target
  piece.foregroundColor = colors.link
  piece.underlineStyle = Text.LineStyle.single
  return piece
}
