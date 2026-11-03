import SwiftUI

/// The loading screen of a cold start, over the app: the TeamCraft mark assembles and types while
/// `isLoading`, and gets out of the way as soon as loading is over. The same choreography as the web's
/// loading screen (crates/teamcraft-ui/style/loader.css, issue #463): the heads swirl in, the
/// bodies rise, the speech bubble pops and types, the team nods along. A screen that was not
/// loading at first never shows it.
struct LoadingMarkOverlay: View {
  let isLoading: Bool
  @Environment(\.accessibilityReduceMotion) private var reduceMotion
  @State private var isShown: Bool
  @State private var start = Date()
  @State private var maybeDepartedAt: Date?

  init(isLoading: Bool) {
    self.isLoading = isLoading
    _isShown = State(initialValue: isLoading)
  }

  var body: some View {
    Group { if isShown { overlay } }
      // Loading again later (a restart of the core): the mark plays again.
      .onChange(of: isLoading) { _, isLoadingNow in
        guard isLoadingNow, !isShown else { return }
        start = Date()
        maybeDepartedAt = nil
        isShown = true
      }
  }

  private var overlay: some View {
    Group {
      TimelineView(.animation) { context in
        let elapsed = context.date.timeIntervalSince(start) * 1000
        let gone = departure(at: context.date)
        ZStack {
          Theme.background.ignoresSafeArea()
          Canvas { canvas, size in
            MarkDrawing(elapsed: elapsed, reduceMotion: reduceMotion).draw(in: &canvas, size: size)
          }
          .frame(width: MarkTiming.size, height: MarkTiming.size)
          .scaleEffect(reduceMotion ? 1 : 1 + (MarkTiming.departScale - 1) * gone)
          .opacity(reduceMotion ? min(1, elapsed / MarkTiming.fadeMs) : 1)
        }
        .opacity(1 - gone)
      }
      .accessibilityElement()
      .accessibilityLabel(Text("loading"))
      // Leaving: no longer announced.
      .accessibilityHidden(maybeDepartedAt != nil)
      .task(id: isLoading) { await handOverWhenLoaded() }
    }
  }

  /// Once loading is over the handover plays and the overlay goes; a fast start is not held back
  /// for the animation.
  private func handOverWhenLoaded() async {
    guard !isLoading else { return }
    maybeDepartedAt = Date()
    try? await Task.sleep(for: .milliseconds(Int(reduceMotion ? MarkTiming.fadeMs : MarkTiming.departMs)))
    guard !Task.isCancelled else { return }
    isShown = false
  }

  private func departure(at date: Date) -> Double {
    guard let departedAt = maybeDepartedAt else { return 0 }
    let duration = reduceMotion ? MarkTiming.fadeMs : MarkTiming.departMs
    return MarkTiming.emphasized.value(at: fraction(date.timeIntervalSince(departedAt) * 1000, 0, duration))
  }
}

/// The TeamCraft mark at rest, drawn like the loading screen's (the sign-in screen's tile).
struct StillMarkView: View {
  var body: some View {
    Canvas { canvas, size in
      MarkDrawing(elapsed: MarkTiming.restingMs, reduceMotion: false).draw(in: &canvas, size: size)
    }
    .aspectRatio(1, contentMode: .fit)
  }
}

/// Times in milliseconds from the first frame, as in loader.css.
private enum MarkTiming {
  static let size: CGFloat = 88
  static let emphasized = UnitCurve.bezier(startControlPoint: UnitPoint(x: 0.05, y: 0.7), endControlPoint: UnitPoint(x: 0.1, y: 1))
  static let pop = UnitCurve.bezier(startControlPoint: UnitPoint(x: 0.34, y: 1.56), endControlPoint: UnitPoint(x: 0.64, y: 1))
  static let easeInOut = UnitCurve.bezier(startControlPoint: UnitPoint(x: 0.42, y: 0), endControlPoint: UnitPoint(x: 0.58, y: 1))
  static let easeOut = UnitCurve.bezier(startControlPoint: UnitPoint(x: 0, y: 0), endControlPoint: UnitPoint(x: 0.58, y: 1))
  static let swirlMs = 820.0
  static let swirlDegrees = -150.0
  static let headStartScale = 0.4
  static let riseMs = 640.0
  static let riseShare = 0.18
  static let popMs = 460.0
  static let popDelayMs = 420.0
  static let popStartScale = 0.6
  static let dotInMs = 160.0
  static let dotRestOpacity = 0.55
  static let hopPeriodMs = 1200.0
  static let hopShare = 0.4
  static let nodPeriodMs = 2400.0
  static let nodShare = 0.06
  /// Every part rests here: assembled, the loops not started yet.
  static let restingMs = 1040.0
  /// The handover: the mark rises towards the viewer and fades with the backdrop.
  static let departMs = 320.0
  static let departScale = 1.15
  /// Reduced motion: no movement, the mark fades in and out.
  static let fadeMs = 240.0
  // Top, left, right head; left, centre, right dot.
  static let headDelaysMs = [0.0, 70, 140]
  static let nodDelaysMs = [1300.0, 1100, 1500]
  static let dotInDelaysMs = [760.0, 820, 880]
  static let hopDelaysMs = [1100.0, 1250, 1400]
}

/// One frame of the mark at `elapsed` milliseconds.
private struct MarkDrawing {
  let elapsed: Double
  let reduceMotion: Bool

  func draw(in canvas: inout GraphicsContext, size: CGSize) {
    let view = MarkParts.view
    canvas.scaleBy(x: size.width / view.width, y: size.width / view.width)
    canvas.translateBy(x: -view.minX, y: -view.minY)
    if reduceMotion {
      drawStill(canvas)
    } else {
      drawMoving(canvas)
    }
  }

  /// Reduced motion: the mark stands still and its dots only brighten in turn.
  private func drawStill(_ canvas: GraphicsContext) {
    fill(MarkParts.bodyRight, canvas)
    fill(MarkParts.bodyTop, canvas)
    fillBodyLeft(canvas)
    for head in [MarkParts.headTop, MarkParts.headLeft, MarkParts.headRight] { fill(head, canvas) }
    fill(MarkParts.bubble, canvas)
    for (index, center) in MarkParts.dots.enumerated() {
      let phase = loopPhase(elapsed, Double(index) * 200, MarkTiming.hopPeriodMs)
      let pulse = phase < 0.5 ? phase * 2 : (1 - phase) * 2
      fillDot(center, opacity: 1 - (1 - MarkTiming.dotRestOpacity) * pulse, canvas)
    }
  }

  private func drawMoving(_ canvas: GraphicsContext) {
    rise(MarkParts.bodyRight, delay: 280, canvas) { fill(MarkParts.bodyRight, $0) }
    rise(MarkParts.bodyTop, delay: 160, canvas) { fill(MarkParts.bodyTop, $0) }
    rise(MarkParts.bodyLeft, delay: 220, canvas) { fillBodyLeft($0) }
    for (index, head) in [MarkParts.headTop, MarkParts.headLeft, MarkParts.headRight].enumerated() {
      drawHead(head, delay: MarkTiming.headDelaysMs[index], nodDelay: MarkTiming.nodDelaysMs[index], canvas)
    }
    drawBubble(canvas)
  }

  /// A body rises from below and fades in.
  private func rise(_ part: MarkPart, delay: Double, _ canvas: GraphicsContext, draw: (GraphicsContext) -> Void) {
    let progress = MarkTiming.emphasized.value(at: fraction(elapsed, delay, MarkTiming.riseMs))
    var context = canvas
    context.translateBy(x: 0, y: part.box.height * MarkTiming.riseShare * (1 - progress))
    context.opacity = progress
    draw(context)
  }

  /// A head swirls in around the mark's centre and grows into place, then nods now and then.
  private func drawHead(_ head: MarkPart, delay: Double, nodDelay: Double, _ canvas: GraphicsContext) {
    let linear = fraction(elapsed, delay, MarkTiming.swirlMs)
    let progress = MarkTiming.emphasized.value(at: linear)
    let grow = MarkTiming.headStartScale + (1 - MarkTiming.headStartScale) * progress
    let nod = bump(loopPhase(elapsed, nodDelay, MarkTiming.nodPeriodMs), half: 0.12) * head.box.height * MarkTiming.nodShare
    var context = canvas
    context.opacity = MarkTiming.emphasized.value(at: min(1, linear / 0.4))
    rotate(&context, degrees: MarkTiming.swirlDegrees * (1 - progress), around: CGPoint(x: MarkParts.view.midX, y: MarkParts.view.midY))
    context.translateBy(x: 0, y: -nod)
    scale(&context, by: grow, around: CGPoint(x: head.box.midX, y: head.box.midY))
    fill(head, context)
  }

  /// The speech bubble pops from its tail, then its dots fade in and hop in turn.
  private func drawBubble(_ canvas: GraphicsContext) {
    let bubble = MarkParts.bubble
    let linear = fraction(elapsed, MarkTiming.popDelayMs, MarkTiming.popMs)
    let pop = MarkTiming.popStartScale + (1 - MarkTiming.popStartScale) * MarkTiming.pop.value(at: linear)
    var context = canvas
    context.opacity = max(0, min(1, MarkTiming.pop.value(at: min(1, linear / 0.4))))
    scale(&context, by: pop, around: CGPoint(x: bubble.box.minX + bubble.box.width * 0.48, y: bubble.box.maxY))
    fill(bubble, context)
    for (index, center) in MarkParts.dots.enumerated() {
      let hopDelay = MarkTiming.hopDelaysMs[index]
      let appear = MarkTiming.easeOut.value(at: fraction(elapsed, MarkTiming.dotInDelaysMs[index], MarkTiming.dotInMs))
      let hop = bump(loopPhase(elapsed, hopDelay, MarkTiming.hopPeriodMs), half: 0.3)
      let opacity = elapsed < hopDelay ? appear * MarkTiming.dotRestOpacity : MarkTiming.dotRestOpacity + (1 - MarkTiming.dotRestOpacity) * hop
      let lift = hop * MarkParts.dotRadius * 2 * MarkTiming.hopShare
      fillDot(CGPoint(x: center.x, y: center.y - lift), opacity: opacity, context)
    }
  }

  /// The left body with its darker upper face.
  private func fillBodyLeft(_ canvas: GraphicsContext) {
    fill(MarkParts.bodyLeft, canvas)
    var shaded = canvas
    shaded.clip(to: MarkParts.bodyLeftShadeClip)
    fill(MarkParts.bodyLeftShade, shaded)
  }

  private func fill(_ part: MarkPart, _ canvas: GraphicsContext) {
    switch part.fill {
    case .solid(let hex):
      canvas.fill(part.path, with: .color(Color(uiColor: UIColor(hex: hex))))
    case .gradient(let hexes, let start, let end):
      let gradient = Gradient(colors: hexes.map { Color(uiColor: UIColor(hex: $0)) })
      canvas.fill(part.path, with: .linearGradient(gradient, startPoint: start, endPoint: end))
    }
  }

  private func fillDot(_ center: CGPoint, opacity: Double, _ canvas: GraphicsContext) {
    var context = canvas
    context.opacity *= opacity
    let radius = MarkParts.dotRadius
    let dot = Path(ellipseIn: CGRect(x: center.x - radius, y: center.y - radius, width: 2 * radius, height: 2 * radius))
    context.fill(dot, with: .color(Color(uiColor: UIColor(hex: MarkParts.dotHex))))
  }

  private func rotate(_ context: inout GraphicsContext, degrees: Double, around pivot: CGPoint) {
    context.translateBy(x: pivot.x, y: pivot.y)
    context.rotate(by: .degrees(degrees))
    context.translateBy(x: -pivot.x, y: -pivot.y)
  }

  private func scale(_ context: inout GraphicsContext, by factor: Double, around pivot: CGPoint) {
    context.translateBy(x: pivot.x, y: pivot.y)
    context.scaleBy(x: factor, y: factor)
    context.translateBy(x: -pivot.x, y: -pivot.y)
  }
}

/// 0 before `delay`, 1 after it plus `duration`, linear between.
private func fraction(_ t: Double, _ delay: Double, _ duration: Double) -> Double {
  max(0, min(1, (t - delay) / duration))
}

/// Where a loop that starts at `delay` stands in its period, 0 until it starts.
private func loopPhase(_ t: Double, _ delay: Double, _ period: Double) -> Double {
  t < delay ? 0 : (t - delay).truncatingRemainder(dividingBy: period) / period
}

/// Up and back down within the first 2 × `half` of the loop (eased both ways), at rest after.
private func bump(_ phase: Double, half: Double) -> Double {
  if phase < half { return MarkTiming.easeInOut.value(at: phase / half) }
  if phase < 2 * half { return 1 - MarkTiming.easeInOut.value(at: (phase - half) / half) }
  return 0
}
