import UIKit
import XCTest

@testable import TeamCraft

final class ComposerFormattingTests: XCTestCase {
  private let palette = ComposerPalette(
    font: .systemFont(ofSize: 17), text: .black, marker: .gray, quote: .darkGray, link: .blue, mention: .cyan,
    codeBackground: .lightGray)

  private func span(_ start: UInt32, _ end: UInt32, _ kind: ComposerSpanKind) -> ComposerSpan {
    ComposerSpan(start: start, end: end, kind: kind)
  }

  private func styled(_ text: String, _ spans: [ComposerSpan]) -> NSMutableAttributedString {
    let storage = NSMutableAttributedString(string: text)
    ComposerStyler.apply(spans, to: storage, palette: palette)
    return storage
  }

  // MARK: Styling

  func testStylingChangesAttributesOnly() {
    let storage = styled("a **b** c", [span(4, 5, .bold), span(2, 4, .marker)])
    XCTAssertEqual(storage.string, "a **b** c")
    let font = storage.attribute(.font, at: 4, effectiveRange: nil) as? UIFont
    XCTAssertTrue(font?.fontDescriptor.symbolicTraits.contains(.traitBold) ?? false)
    XCTAssertEqual(storage.attribute(.foregroundColor, at: 2, effectiveRange: nil) as? UIColor, .gray)
    XCTAssertEqual(storage.attribute(.foregroundColor, at: 6, effectiveRange: nil) as? UIColor, .black)
  }

  func testBoldInsideItalicKeepsBoth() {
    let storage = styled("_a **b**_", [span(0, 9, .italic), span(5, 6, .bold)])
    let traits = (storage.attribute(.font, at: 5, effectiveRange: nil) as? UIFont)?.fontDescriptor.symbolicTraits
    XCTAssertTrue(traits?.contains(.traitBold) ?? false)
    XCTAssertTrue(traits?.contains(.traitItalic) ?? false)
  }

  func testCodeIsMonospacedOnABackground() {
    let storage = styled("`x`", [span(1, 2, .code)])
    let font = storage.attribute(.font, at: 1, effectiveRange: nil) as? UIFont
    XCTAssertTrue(font?.fontDescriptor.symbolicTraits.contains(.traitMonoSpace) ?? false)
    XCTAssertEqual(storage.attribute(.backgroundColor, at: 1, effectiveRange: nil) as? UIColor, .lightGray)
  }

  func testStrikeAndLinkDecorations() {
    let storage = styled("~~a~~ b", [span(2, 3, .strike), span(6, 7, .link)])
    XCTAssertNotNil(storage.attribute(.strikethroughStyle, at: 2, effectiveRange: nil))
    XCTAssertNotNil(storage.attribute(.underlineStyle, at: 6, effectiveRange: nil))
    XCTAssertEqual(storage.attribute(.foregroundColor, at: 6, effectiveRange: nil) as? UIColor, .blue)
  }

  func testSpansPastTheEndAreClamped() {
    let storage = styled("ab", [span(1, 9, .bold), span(7, 9, .bold)])
    XCTAssertEqual(storage.string, "ab")
  }

  func testRestylingStartsFromPlain() {
    let storage = styled("ab", [span(0, 2, .bold)])
    ComposerStyler.apply([], to: storage, palette: palette)
    let traits = (storage.attribute(.font, at: 0, effectiveRange: nil) as? UIFont)?.fontDescriptor.symbolicTraits
    XCTAssertFalse(traits?.contains(.traitBold) ?? true)
  }

  // MARK: Applying the core's edit

  func testTheDiffIsTheSmallestReplacement() {
    XCTAssertEqual(
      ComposerDiff.change(from: "a word b", to: "a **word** b"),
      ComposerChange(range: NSRange(location: 2, length: 4), replacement: "**word**"))
  }

  func testTheDiffOfAnInsertionInTheMiddle() {
    let change = ComposerDiff.change(from: "ab", to: "a**b")
    XCTAssertEqual(change, ComposerChange(range: NSRange(location: 1, length: 0), replacement: "**"))
  }

  func testTheDiffOfARemoval() {
    let change = ComposerDiff.change(from: "- one", to: "one")
    XCTAssertEqual(change, ComposerChange(range: NSRange(location: 0, length: 2), replacement: ""))
  }

  func testNoChangeIsAnEmptyReplacement() {
    let change = ComposerDiff.change(from: "same", to: "same")
    XCTAssertEqual(change.range.length, 0)
    XCTAssertEqual(change.replacement, "")
  }

  func testTheDiffNeverSplitsASurrogatePair() {
    // 😀 and 😁 share their first UTF-16 unit.
    let change = ComposerDiff.change(from: "a😀b", to: "a😁b")
    XCTAssertEqual(change.range, NSRange(location: 1, length: 2))
    XCTAssertEqual(change.replacement, "😁")
  }

  func testTheDiffAppliedGivesTheNewText() {
    let pairs = [("a**b", "a b"), ("", "> "), ("x\n- ", "x\n"), ("é", "e\u{301}"), ("aa", "aaa")]
    for (old, new) in pairs {
      let change = ComposerDiff.change(from: old, to: new)
      let result = (old as NSString).replacingCharacters(in: change.range, with: change.replacement)
      XCTAssertEqual(Array(result.utf16), Array(new.utf16), "\(old) -> \(new)")
    }
  }

  // MARK: Keys

  private func edit(_ text: String, at caret: UInt32) -> ComposerEdit {
    ComposerEdit(text: text, selectionStart: caret, selectionEnd: caret)
  }

  func testReturnAtACaretAsksTheCore() {
    var seen: (String, UInt32)?
    let result = ComposerKeys.edit(
      text: "- one", selection: NSRange(location: 5, length: 0), range: NSRange(location: 5, length: 0),
      replacement: "\n",
      enter: { text, caret in seen = (text, caret); return self.edit("- one\n- ", at: 8) },
      backspace: { _, _ in XCTFail("not a backspace"); return nil })
    XCTAssertEqual(seen?.0, "- one")
    XCTAssertEqual(seen?.1, 5)
    XCTAssertEqual(result?.text, "- one\n- ")
  }

  func testBackspaceRemovesTheCharBeforeTheCaret() {
    var seen: UInt32?
    let result = ComposerKeys.edit(
      text: "- ", selection: NSRange(location: 2, length: 0), range: NSRange(location: 1, length: 1),
      replacement: "",
      enter: { _, _ in XCTFail("not a return"); return nil },
      backspace: { _, caret in seen = caret; return self.edit("", at: 0) })
    XCTAssertEqual(seen, 2)
    XCTAssertEqual(result?.text, "")
  }

  func testASelectionIsNotAKey() {
    let result = ComposerKeys.edit(
      text: "abc", selection: NSRange(location: 0, length: 3), range: NSRange(location: 0, length: 3),
      replacement: "\n",
      enter: { _, _ in XCTFail("selection"); return nil },
      backspace: { _, _ in XCTFail("selection"); return nil })
    XCTAssertNil(result)
  }

  func testTypingAndPastingAreNoKeys() {
    let nothing: ComposerKeys.Handler = { _, _ in XCTFail("not a key"); return nil }
    XCTAssertNil(
      ComposerKeys.edit(
        text: "a", selection: NSRange(location: 1, length: 0), range: NSRange(location: 1, length: 0),
        replacement: "b", enter: nothing, backspace: nothing))
    XCTAssertNil(
      ComposerKeys.edit(
        text: "a", selection: NSRange(location: 1, length: 0), range: NSRange(location: 1, length: 0),
        replacement: "x\ny\n", enter: nothing, backspace: nothing))
  }

  func testACoreAnswerOfNilLetsTheKeyThrough() {
    XCTAssertNil(
      ComposerKeys.edit(
        text: "plain", selection: NSRange(location: 5, length: 0), range: NSRange(location: 5, length: 0),
        replacement: "\n", enter: { _, _ in nil }, backspace: { _, _ in nil }))
  }

  // MARK: Names

  func testEveryFormatHasANameAndASymbol() {
    for format in ComposerFormatting.formats {
      XCTAssertFalse(ComposerFormatting.name(of: format).isEmpty)
      XCTAssertNotNil(UIImage(systemName: ComposerFormatting.symbol(of: format)), "\(format)")
    }
    XCTAssertEqual(Set(ComposerFormatting.formats).count, ComposerFormatting.formats.count)
  }
}
