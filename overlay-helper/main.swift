// VoiceDumps dictation overlay — a standalone accessory process.
//
// Why a separate process at all: a Tauri/tao webview window cannot enter another
// app's active full-screen Space, no matter its window level or collection
// behaviour (measured exhaustively). A native accessory NSPanel can. So the pill
// the user sees while dictating lives here, and the main app drives it over a
// pipe.
//
// Design: QWEE V2 "Field Notes" — forest & sage, squares not circles, the
// pixel-cluster brand mark, and stepped/mechanical motion. The one concession is
// the rounded frosted container: a hard rectangle floating over another app's UI
// reads as a system error, a soft frosted pill reads as a quiet status HUD.
//
// Protocol — one command per line on stdin:
//   show / transcribing / level <0..1> / text <words> / hide / quit

import Cocoa
import QuartzCore

// MARK: - Palette (QWEE V2 §3)

private let cForestTint = NSColor(calibratedRed: 0.09, green: 0.12, blue: 0.07, alpha: 0.55)
private let cBorder = NSColor(calibratedRed: 0.72, green: 0.83, blue: 0.64, alpha: 0.20)
private let cSage = NSColor(calibratedRed: 0.72, green: 0.83, blue: 0.64, alpha: 1)
private let cSageDim = NSColor(calibratedRed: 0.56, green: 0.69, blue: 0.49, alpha: 1)
private let cSageBright = NSColor(calibratedRed: 0.82, green: 0.90, blue: 0.76, alpha: 1)
/// Deeper than `cSageDim`, and only used for the cells of an uncertain word.
///
/// Squares lay down more ink than the letters they replace, so a pixelated word
/// drawn in the reading colour comes out *louder* than the sentence around it —
/// which says the opposite of what it means. Pulled down to here it sits under
/// its neighbours, and the eye reads it as something held back.
private let cSageDeep = NSColor(calibratedRed: 0.40, green: 0.50, blue: 0.34, alpha: 1)

// The brand mark — §4.4 pixel cluster, 3×3 with two cells knocked out.
private let brand: [Bool] = [true, true, false, true, true, true, false, true, true]
// Reveal / pulse order: centre first, then rolling outward. Indices into 0..8,
// covering exactly the lit cells. Kept fixed so motion is deliberate, not random.
private let clusterOrder: [Int] = [4, 1, 3, 5, 7, 0, 8]

// MARK: - Pill view (drawn directly; squares stay square)

final class PillView: NSView {
    enum Mode { case recording, transcribing }

    // Compact, left-anchored layout. The pill sizes itself to this content.
    private let barCount = 13
    private let cellSize: CGFloat = 3.0
    private let cellGap: CGFloat = 1.6
    private let barW: CGFloat = 2.0
    private let barGap: CGFloat = 2.6
    private let maxBarH: CGFloat = 13
    private let leftPad: CGFloat = 15
    private let midGap: CGFloat = 11
    private let rightPad: CGFloat = 15

    private var levels: [CGFloat]
    private var incoming: CGFloat = 0
    private var smoothLevel: CGFloat = 0
    private var phase: CGFloat = 0
    private var shownAt = CFAbsoluteTimeGetCurrent()
    private var startedAt = CFAbsoluteTimeGetCurrent()
    private(set) var mode: Mode = .recording
    private var orderOf: [Int: Int] = [:]

    override init(frame: NSRect) {
        levels = Array(repeating: 0, count: barCount)
        super.init(frame: frame)
        wantsLayer = true
        for (slot, cell) in clusterOrder.enumerated() { orderOf[cell] = slot }
    }
    required init?(coder: NSCoder) { fatalError() }

    override var isFlipped: Bool { false }

    private var clusterExtent: CGFloat { 3 * cellSize + 2 * cellGap }
    private var barsWidth: CGFloat { CGFloat(barCount) * barW + CGFloat(barCount - 1) * barGap }

    private let labelText = "TRANSCRIBING"
    private var labelAttrs: [NSAttributedString.Key: Any] {
        [
            .font: NSFont.monospacedSystemFont(ofSize: 10, weight: .medium),
            .foregroundColor: cSageBright,
            .kern: 1.5,
        ]
    }

    /// The width the pill should be for a given state — it hugs its content.
    func contentWidth(_ m: Mode) -> CGFloat {
        let head = leftPad + clusterExtent + midGap
        switch m {
        case .recording:
            let timeW: CGFloat = 20 // room for "88s"
            return head + barsWidth + midGap + timeW + rightPad
        case .transcribing:
            let w = NSAttributedString(string: labelText, attributes: labelAttrs).size().width
            return head + ceil(w) + rightPad
        }
    }

    func setMode(_ m: Mode) {
        mode = m
        shownAt = CFAbsoluteTimeGetCurrent()
        if m == .recording {
            startedAt = shownAt
            for i in levels.indices { levels[i] = 0 }
        }
        needsDisplay = true
    }

    func setLevel(_ v: CGFloat) { incoming = max(0, min(1, v)) }

    func tick() {
        phase += 0.10
        smoothLevel += (incoming - smoothLevel) * 0.25
        if mode == .recording {
            for i in 0..<(barCount - 1) { levels[i] = max(levels[i + 1] * 0.85, 0) }
            levels[barCount - 1] = incoming
        }
        needsDisplay = true
    }

    /// 0→1 over the first 0.34s after a state change — drives the cell stagger.
    private func entrance() -> CGFloat {
        CGFloat(min(1, max(0, (CFAbsoluteTimeGetCurrent() - shownAt) / 0.34)))
    }

    override func draw(_ dirtyRect: NSRect) {
        let b = bounds
        NSBezierPath(roundedRect: b, xRadius: b.height / 2, yRadius: b.height / 2).addClip()
        cForestTint.setFill()
        b.fill()

        drawCluster(b)
        let contentX = leftPad + clusterExtent + midGap
        if mode == .recording {
            drawBars(x: contentX, b)
            drawTime(b)
        } else {
            drawLabel(x: contentX, b)
        }
    }

    // The logo. Cells snap in on show; in recording they brighten with your
    // voice; in transcribing they roll through a blink pulse — the brand's
    // "thinking" indicator, never a spinner.
    private func drawCluster(_ b: NSRect) {
        let oy = b.height / 2 - clusterExtent / 2
        let e = entrance()
        for i in 0..<9 where brand[i] {
            let slot = orderOf[i] ?? 0
            let threshold = CGFloat(slot) / CGFloat(clusterOrder.count)
            if e < threshold { continue }
            let appear = min(1, (e - threshold) * 5)
            let base: CGFloat
            switch mode {
            case .recording:
                base = 0.62 + 0.38 * min(1, smoothLevel * 1.4) // logo reacts to speech
            case .transcribing:
                let w = 0.5 + 0.5 * sin(phase * 3.0 - CGFloat(slot) * 0.7)
                base = 0.28 + 0.62 * w
            }
            cSage.withAlphaComponent(base * appear).setFill()
            let col = i % 3
            let row = i / 3
            let x = leftPad + CGFloat(col) * (cellSize + cellGap)
            let y = oy + CGFloat(2 - row) * (cellSize + cellGap) // row 0 = top
            CGRect(x: x, y: y, width: cellSize, height: cellSize).fill()
        }
    }

    // Square bars, quietly alive even in silence.
    private func drawBars(x startX: CGFloat, _ b: NSRect) {
        let e = entrance()
        for i in 0..<barCount {
            let idle = 0.05 * (0.5 + 0.5 * sin(phase * 1.6 + CGFloat(i) * 0.55))
            let lv = max(levels[i], idle) * e
            let h = 2 + lv * maxBarH
            let x = startX + CGFloat(i) * (barW + barGap)
            cSage.withAlphaComponent(0.30 + lv * 0.68).setFill()
            CGRect(x: x, y: b.height / 2 - h / 2, width: barW, height: h).fill()
        }
    }

    private func drawTime(_ b: NSRect) {
        let t = CFAbsoluteTimeGetCurrent() - startedAt
        let s = String(format: "%.0fs", t)
        let attrs: [NSAttributedString.Key: Any] = [
            .font: NSFont.monospacedDigitSystemFont(ofSize: 10, weight: .medium),
            .foregroundColor: cSageDim,
        ]
        let str = NSAttributedString(string: s, attributes: attrs)
        let sz = str.size()
        str.draw(at: CGPoint(x: b.width - sz.width - rightPad, y: b.height / 2 - sz.height / 2))
    }

    private func drawLabel(x: CGFloat, _ b: NSRect) {
        let str = NSAttributedString(string: labelText, attributes: labelAttrs)
        str.draw(at: CGPoint(x: x, y: b.height / 2 - str.size().height / 2))
    }
}

// MARK: - Live transcript

/// One word of the live transcript, and how sure the model was of it.
struct Heard {
    let word: String
    /// 0…1. The wire carries one digit per word, so this arrives in ninths.
    let confidence: CGFloat
    /// Whether `medium` has been back over this word. The fast model is what
    /// puts words on screen; this is what turns a guess into the sentence.
    var refined: Bool = false

    /// How broken up this word was just before it was corrected, and when the
    /// correction landed.
    ///
    /// Carried on the word rather than tracked as a span of indices, because
    /// `fit()` drops words off the front and splices an ellipsis in — any index
    /// into `words` stops meaning anything by the time it reaches `draw`.
    var wasDoubt: CGFloat = 0
    var resolvedAt: CFAbsoluteTime? = nil

    /// How much of this word to render as cells right now.
    ///
    /// Ordinarily that is just its doubt. In the moment after a correction it
    /// is whatever it was before, easing down to what it is now — so a word
    /// that `medium` has just vouched for is seen to resolve rather than being
    /// swapped out between frames.
    func drawnDoubt(_ now: CFAbsoluteTime, over duration: CFTimeInterval) -> CGFloat {
        let settled = TranscriptView.doubt(confidence)
        guard let at = resolvedAt, wasDoubt > settled else { return settled }
        let p = min(1, max(0, (now - at) / duration))
        let eased = 1 - pow(1 - p, 3)
        return max(settled, wasDoubt * (1 - eased))
    }
}

/// Decode a `text` payload.
///
/// Words separated by spaces, each prefixed with a single digit for its
/// confidence in ninths — see `overlay::words` on the Rust side. An optional
/// `*` in front of the digit marks a word `medium` has already re-read. A field
/// that is only a digit, or that starts with something else, is dropped rather
/// than guessed at: a malformed line should cost one word, not the whole panel.
func parseHeard(_ payload: String) -> [Heard] {
    payload.split(separator: " ").compactMap { field in
        var field = field
        let refined = field.first == "*"
        if refined { field = field.dropFirst() }
        guard field.count > 1, let digit = field.first?.wholeNumberValue,
            (0...9).contains(digit)
        else { return nil }
        return Heard(
            word: String(field.dropFirst()), confidence: CGFloat(digit) / 9.0, refined: refined)
    }
}

/// A TextKit stack for one string at one width.
///
/// Everything the panel needs — the size it wants, the lines it took, and where
/// each word landed — comes out of the same layout, so the pixel cells drawn
/// over an uncertain word cannot drift away from the glyphs they stand in for.
final class Layout {
    let storage: NSTextStorage
    let manager = NSLayoutManager()
    let container: NSTextContainer

    init(_ s: NSAttributedString, width: CGFloat) {
        storage = NSTextStorage(attributedString: s)
        container = NSTextContainer(
            size: NSSize(width: width, height: .greatestFiniteMagnitude))
        container.lineFragmentPadding = 0
        manager.usesFontLeading = true
        manager.addTextContainer(container)
        storage.addLayoutManager(manager)
        manager.ensureLayout(for: container)
    }

    var glyphs: NSRange { manager.glyphRange(for: container) }

    var size: NSSize {
        let r = manager.usedRect(for: container)
        return NSSize(width: ceil(r.maxX), height: ceil(r.maxY))
    }

    var lines: Int {
        var n = 0
        var i = 0
        while i < manager.numberOfGlyphs {
            var r = NSRange()
            _ = manager.lineFragmentRect(forGlyphAt: i, effectiveRange: &r)
            i = max(NSMaxRange(r), i + 1)
            n += 1
        }
        return max(1, n)
    }
}

/// A scratch view used only to rasterise one word so its ink can be measured.
///
/// A view rather than an image with focus locked on it: `cacheDisplay(in:to:)`
/// renders at one sample per point regardless of which display the panel is on,
/// so a word breaks into the same cells on a laptop and on an external monitor.
final class WordCanvas: NSView {
    var render: (() -> Void)?
    override var isFlipped: Bool { true }
    override func draw(_ dirtyRect: NSRect) { render?() }
}

/// One word broken into square cells, ready to be drawn instead of its glyphs.
struct Grid {
    let cols: Int
    let rows: Int
    /// Side of one cell, in points.
    let cell: CGFloat
    /// Ink coverage, 0…1, per cell — row 0 at the top.
    let coverage: [CGFloat]
    /// Top of the grid down to the text baseline, so the cells can be hung off
    /// the same baseline the layout gave the glyphs.
    let ascent: CGFloat
}

/// What has been heard so far, shown above the pill while you speak.
///
/// A separate panel rather than more pill: the pill is a status light — logo,
/// level, elapsed time — and it earns its shape by staying that size. Words are
/// a different kind of thing and want to wrap, so they get their own surface
/// and the pill is left exactly as it was.
///
/// Deliberately quieter than the pill: dimmer text on a fainter ground. It is a
/// reassurance that the right words are being heard, not something to read — you
/// are looking at the app you are dictating into, not at this.
final class TranscriptView: NSView {
    /// Wide enough for a sentence to breathe, narrow enough to stay a HUD.
    static let maxWidth: CGFloat = 520
    /// Beyond this the panel would dominate the screen, so older lines scroll
    /// off the top instead.
    ///
    /// Six rather than four: at four, a sentence of any length was already
    /// sliding off the top while it was still being said, and the panel spent
    /// most of a dictation showing the end of a thought with no beginning. Six
    /// is about as tall as this can get and still read as a HUD sitting over
    /// your work rather than a window of its own.
    static let maxLines = 6
    static let padX: CGFloat = 16
    static let padY: CGFloat = 11
    /// The dissolve needs vertical room to work in. At 13pt an x-height is
    /// barely nine device pixels — three cells — and a short uncertain word
    /// stops being a word rather than fraying at its edges. 16 gives it four or
    /// five, and the panel is still a HUD rather than a document.
    static let fontSize: CGFloat = 16

    private(set) var words: [Heard] = []
    /// How many of `words` had finished arriving before the last update.
    /// Everything past this is the newest chunk, and is what gets animated in.
    private var settled: Int = 0
    /// The tail of `words` that fits in `maxLines`. Recomputed when the words
    /// change rather than per frame: finding it costs a layout per candidate
    /// word, and the panel repaints sixty times a second while text is landing.
    private var shown: [Heard] = []
    private var arrivedAt = CFAbsoluteTimeGetCurrent()
    /// How long a freshly-transcribed chunk takes to arrive.
    private let revealDuration: CFTimeInterval = 0.34
    /// And how long it then glows before joining the rest.
    ///
    /// The two-stage treatment is the point: words are brightest as they land
    /// and ease back to the weight of everything already said. It gives the
    /// panel a reading edge — your eye knows where the new words are without
    /// hunting — and it is what makes this feel like speech being heard rather
    /// than a text field being rewritten.
    private let settleDuration: CFTimeInterval = 0.85
    /// How long a corrected word takes to resolve.
    ///
    /// Slower than the reveal on purpose. An arrival should feel like a word
    /// being caught; a correction should feel like one coming into focus, and
    /// at 0.34s the cells snap off rather than dissolve. It is also the only
    /// motion on screen that is not tied to the voice, so it should not compete
    /// with the edge of the sentence for attention.
    static let resolveDuration: CFTimeInterval = 0.55

    /// Text that has already been said and read: the duller green, and dimmer.
    ///
    /// Settled and fresh differ in hue as well as brightness. Alpha alone is
    /// too quiet a signal at this size — the eye reads a colour change long
    /// before it reads a 40% opacity difference — and the point of the whole
    /// treatment is that you can see where the new words are without hunting
    /// for them.
    private let restColor = cSageDim
    private let restAlpha: CGFloat = 0.66
    /// Text that just landed: the bright green, at full strength.
    private let freshColor = cSageBright
    private let freshAlpha: CGFloat = 1.0

    /// Whether the reveal is still running, so the controller knows to keep
    /// redrawing. Words appear a chunk at a time — without this they would pop
    /// in fully formed on a single frame, which reads as a glitch rather than
    /// as speech being heard.
    var isAnimating: Bool {
        guard !words.isEmpty else { return false }
        let now = CFAbsoluteTimeGetCurrent()
        if now - arrivedAt < revealDuration + settleDuration { return true }
        // A correction can land while the panel is otherwise still, so its
        // window keeps the frames coming on its own.
        return words.contains {
            guard let at = $0.resolvedAt else { return false }
            return now - at < TranscriptView.resolveDuration
        }
    }

    override var isFlipped: Bool { true }

    static var font: NSFont { NSFont.systemFont(ofSize: fontSize, weight: .regular) }

    static var attrs: [NSAttributedString.Key: Any] {
        let para = NSMutableParagraphStyle()
        para.lineBreakMode = .byWordWrapping
        para.lineSpacing = 3
        return [
            .font: font,
            // Base is the settled weight; the newest run overrides it.
            .foregroundColor: cSageDim.withAlphaComponent(0.66),
            .paragraphStyle: para,
        ]
    }

    // MARK: Confidence

    /// The side of one cell, in points.
    ///
    /// Fixed, and deliberately small: at 16pt type an x-height is a little over
    /// four cells, which is enough to keep a word's shape and not enough to
    /// pretend the letters are still there. Coarser cells were tried first and
    /// they do not degrade a word, they delete it — "off" became a smudge.
    static let cell: CGFloat = 2

    /// How far below the line of trust a word has fallen: 0 keeps its glyphs,
    /// 1 is as broken up as anything gets.
    ///
    /// Doubt drives how much a word erodes rather than how big its cells are.
    /// Growing the cells destroys short words first, which is backwards — the
    /// short ones are where the model's mistakes actually live.
    ///
    /// The 0.80 line was set by measuring a real dictation against what was
    /// said: marking everything below it catches every word the model got
    /// wrong, and the price is texturing roughly one correct word in nine.
    /// Erring that way is deliberate — a false mark costs a glance, and a
    /// missed error is the thing the preview is being distrusted for.
    static func doubt(_ confidence: CGFloat) -> CGFloat {
        guard confidence < 0.80 else { return 0 }
        return min(1, (0.80 - confidence) / 0.55)
    }

    /// Stable per-cell jitter, so a word's fringe erodes unevenly.
    ///
    /// A flat threshold takes the same amount off every edge, which just makes
    /// the word lighter — it reads as a thinner font rather than as something
    /// coming apart. It is a hash rather than a random number because the panel
    /// repaints sixty times a second and a word that boils is unreadable in a
    /// way a word that has broken up is not.
    private static func jitter(_ i: Int, _ salt: Int) -> CGFloat {
        var h = UInt32(truncatingIfNeeded: (i &+ 1) &* 2_654_435_761 &+ salt &* 40_503)
        h ^= h >> 15
        h = h &* 2_246_822_519
        h ^= h >> 13
        return CGFloat(h % 1024) / 1024
    }

    /// Cell grids, keyed by the word they were measured from.
    ///
    /// A word's grid depends only on its letters and the size of a cell — not
    /// on where it ended up — so it survives every rewrap and every repaint.
    /// Without this the panel would rasterise the same handful of words sixty
    /// times a second.
    private static var grids: [String: Grid] = [:]

    /// Break one word into cells of ink coverage.
    ///
    /// The word is laid out and rendered once, then read back and averaged over
    /// squares. Coverage rather than a threshold, so a stroke that only clips a
    /// cell still registers there — that is what lets the shape erode at its
    /// edges instead of dropping out in whole blocks.
    static func grid(for word: String) -> Grid? {
        if let cached = grids[word] { return cached }

        // White, so every sample read back is coverage and nothing else; the
        // colour the word is actually drawn in is applied per cell later.
        var ink = attrs
        ink[.foregroundColor] = NSColor.white
        let layout = Layout(NSAttributedString(string: word, attributes: ink), width: 10_000)
        guard layout.manager.numberOfGlyphs > 0 else { return nil }

        let used = layout.manager.usedRect(for: layout.container)
        let w = max(1, ceil(used.maxX))
        let h = max(1, ceil(used.maxY))
        let frag = layout.manager.lineFragmentRect(forGlyphAt: 0, effectiveRange: nil)
        let ascent = frag.minY + layout.manager.location(forGlyphAt: 0).y

        let canvas = WordCanvas(frame: NSRect(x: 0, y: 0, width: w, height: h))
        canvas.render = { layout.manager.drawGlyphs(forGlyphRange: layout.glyphs, at: .zero) }
        guard let rep = canvas.bitmapImageRepForCachingDisplay(in: canvas.bounds) else {
            return nil
        }
        canvas.cacheDisplay(in: canvas.bounds, to: rep)

        guard rep.bitsPerSample == 8, rep.samplesPerPixel == 4, let data = rep.bitmapData
        else { return nil }

        let wide = rep.pixelsWide
        let high = rep.pixelsHigh
        let step = max(1, Int((TranscriptView.cell * CGFloat(wide) / w).rounded()))
        let cols = (wide + step - 1) / step
        let rows = (high + step - 1) / step
        let alphaAt = rep.bitmapFormat.contains(.alphaFirst) ? 0 : rep.samplesPerPixel - 1
        let pixel = rep.bitsPerPixel / 8

        var coverage = [CGFloat](repeating: 0, count: cols * rows)
        for cy in 0..<rows {
            for cx in 0..<cols {
                var sum = 0
                var n = 0
                for y in (cy * step)..<min((cy + 1) * step, high) {
                    let row = y * rep.bytesPerRow
                    for x in (cx * step)..<min((cx + 1) * step, wide) {
                        sum += Int(data[row + x * pixel + alphaAt])
                        n += 1
                    }
                }
                coverage[cy * cols + cx] = n > 0 ? CGFloat(sum) / CGFloat(n * 255) : 0
            }
        }

        let built = Grid(
            cols: cols, rows: rows, cell: TranscriptView.cell, coverage: coverage, ascent: ascent)
        // A dictation's vocabulary is small, but the process outlives every
        // dictation. Dropping the lot is fine — the words on screen are
        // re-rasterised on the next frame and nothing else needs them.
        if grids.count > 512 { grids.removeAll() }
        grids[word] = built
        return built
    }

    // MARK: Layout

    /// The tail of the transcript that fits in `maxLines`, with an ellipsis in
    /// front when anything was dropped.
    ///
    /// The tail, because while you are still talking the words that just
    /// arrived are the ones worth seeing; the opening has already done its job
    /// of telling you it heard you correctly.
    private func fit() -> [Heard] {
        guard !words.isEmpty else { return [] }
        var best: [Heard] = []
        var take = 1
        while take <= words.count {
            let tail = Array(words.suffix(take))
            let dropped = take < words.count
            let candidate = dropped ? [Heard(word: "…", confidence: 1)] + tail : tail
            let line = candidate.map(\.word).joined(separator: " ")
            if TranscriptView.lines(line) > TranscriptView.maxLines { break }
            best = candidate
            take += 1
        }
        return best
    }

    private static func lines(_ s: String) -> Int {
        guard !s.isEmpty else { return 0 }
        return Layout(NSAttributedString(string: s, attributes: attrs), width: maxWidth).lines
    }

    /// The panel size these words want, or `.zero` when there is nothing to show.
    func fittingSize() -> NSSize {
        guard !shown.isEmpty else { return .zero }
        let m = Layout(compose(shown).0, width: TranscriptView.maxWidth).size
        return NSSize(
            width: min(m.width, TranscriptView.maxWidth) + TranscriptView.padX * 2,
            height: m.height + TranscriptView.padY * 2)
    }

    func setWords(_ list: [Heard]) {
        var list = list
        let now = CFAbsoluteTimeGetCurrent()

        // Two different events arrive down this one path, and they want
        // opposite treatment. `small` appends a chunk to the end — that is new
        // speech and should reveal. `medium` replaces a chunk in the middle
        // with a better reading of the same speech — nothing new was said, so
        // re-revealing the tail every time a correction landed would make the
        // panel flash on words the user had already read.
        let appended =
            !words.isEmpty && list.count > words.count
            && zip(words, list).allSatisfy { $0.word == $1.word }

        if !appended && !words.isEmpty && !list.isEmpty {
            // The smallest span that actually changed: match forward from the
            // start and backward from the end, and what is left in between is
            // the correction.
            var lo = 0
            while lo < min(words.count, list.count), words[lo].word == list[lo].word,
                words[lo].refined == list[lo].refined
            {
                lo += 1
            }
            var hi = 0
            while hi < min(words.count, list.count) - lo,
                words[words.count - 1 - hi].word == list[list.count - 1 - hi].word
            {
                hi += 1
            }
            // Each corrected word inherits how broken up the word standing in
            // its place was, so it has something to resolve *from*.
            for i in lo..<max(lo, list.count - hi) where list[i].refined {
                let before = i < words.count ? TranscriptView.doubt(words[i].confidence) : 0
                list[i].wasDoubt = before
                list[i].resolvedAt = now
            }
        }

        // Corrections carry their own animation, so nothing in them is treated
        // as freshly spoken. An empty panel is the exception: the opening chunk
        // of a dictation is all new, and has to reveal.
        if appended {
            settled = words.count
        } else if words.isEmpty {
            settled = 0
        } else {
            settled = list.count
        }
        words = list
        shown = fit()
        // Rasterise the doubtful words now rather than during the first paint
        // that needs them. Words land every couple of seconds and the panel
        // repaints sixty times a second, so this is the moment there is time.
        for h in shown where h.drawnDoubt(now, over: TranscriptView.resolveDuration) > 0 {
            _ = TranscriptView.grid(for: h.word)
        }
        arrivedAt = CFAbsoluteTimeGetCurrent()
        needsDisplay = true
    }

    /// The visible words as one attributed string, plus the character range each
    /// one occupies.
    ///
    /// One string rather than a draw per word: the paragraph wraps, and words
    /// laid out separately would not agree with each other about where the line
    /// breaks or how the pairs kern.
    private func compose(_ list: [Heard]) -> (NSAttributedString, [NSRange]) {
        let line = NSMutableString()
        var ranges: [NSRange] = []
        for h in list {
            if line.length > 0 { line.append(" ") }
            let start = line.length
            line.append(h.word)
            ranges.append(NSRange(location: start, length: line.length - start))
        }

        let out = NSMutableAttributedString(
            string: line as String, attributes: TranscriptView.attrs)
        // How many of the *visible* words are new. `fit()` may have dropped
        // words off the front, so this is counted from the end, where the
        // arrivals are.
        let fresh = min(max(words.count - settled, 0), list.count)
        guard fresh > 0 else { return (out, ranges) }

        let age = CFAbsoluteTimeGetCurrent() - arrivedAt
        let from = ranges[list.count - fresh].location
        let range = NSRange(location: from, length: out.length - from)

        // `t` runs 0 (just landed, bright) → 1 (settled, joined the rest).
        let t: CGFloat
        let alpha: CGFloat
        let rise: CGFloat
        if age < revealDuration {
            // Arriving: fade up to full brightness, rising a whisker as it lands.
            let p = min(1, max(0, age / revealDuration))
            let eased = 1 - pow(1 - p, 3)
            t = 0
            alpha = freshAlpha * eased
            rise = -2.0 * (1 - eased)
        } else {
            // Landed: ease back toward the words already said, so the bright
            // edge follows your voice instead of piling up.
            let p = min(1, max(0, (age - revealDuration) / settleDuration))
            t = p * p * (3 - 2 * p)  // smoothstep
            alpha = freshAlpha + (restAlpha - freshAlpha) * t
            rise = 0
        }

        let colour = freshColor.blended(withFraction: t, of: restColor) ?? freshColor
        out.addAttribute(
            .foregroundColor, value: colour.withAlphaComponent(alpha), range: range)
        if rise != 0 {
            out.addAttribute(.baselineOffset, value: rise, range: range)
        }
        return (out, ranges)
    }

    // MARK: Drawing

    override func draw(_ dirtyRect: NSRect) {
        // The frosted view behind this is the ground; all that is added here is
        // a wash of forest so sage text keeps its contrast over a bright
        // background as well as a dark one.
        cForestTint.withAlphaComponent(0.46).setFill()
        bounds.fill()

        let list = shown
        guard !list.isEmpty else { return }
        let (string, ranges) = compose(list)
        // Laid out at the same width it was measured at, not at the width the
        // panel ended up: the panel hugs the text, and re-wrapping to that
        // could push a trailing word onto a line that no longer exists.
        let layout = Layout(string, width: TranscriptView.maxWidth)
        let origin = NSPoint(x: TranscriptView.padX, y: TranscriptView.padY)

        // Which words are drawn as cells rather than glyphs.
        var pixelated: [(range: NSRange, grid: Grid, colour: NSColor, doubt: CGFloat)] = []
        let now = CFAbsoluteTimeGetCurrent()
        for (i, h) in list.enumerated() {
            let doubt = h.drawnDoubt(now, over: TranscriptView.resolveDuration)
            guard doubt > 0 else { continue }
            let range = ranges[i]
            let glyphs = layout.manager.glyphRange(
                forCharacterRange: range, actualCharacterRange: nil)
            guard glyphs.length > 0 else { continue }
            // A word long enough to wrap has no single baseline to hang cells
            // off, so it keeps its glyphs. At this width that is a word of forty
            // characters, which is not a word.
            let first = layout.manager.lineFragmentRect(
                forGlyphAt: glyphs.location, effectiveRange: nil)
            let last = layout.manager.lineFragmentRect(
                forGlyphAt: NSMaxRange(glyphs) - 1, effectiveRange: nil)
            guard first == last, let grid = TranscriptView.grid(for: h.word) else { continue }
            let colour =
                (string.attribute(.foregroundColor, at: range.location, effectiveRange: nil)
                    as? NSColor) ?? restColor.withAlphaComponent(restAlpha)
            pixelated.append((range, grid, colour, doubt))
        }

        // Glyphs everywhere except where cells are about to go. Drawing the lot
        // and painting over it would mean punching a hole in a translucent
        // panel, and the covered glyphs would ghost through.
        var cursor = 0
        for word in pixelated {
            if word.range.location > cursor {
                draw(layout, NSRange(location: cursor, length: word.range.location - cursor), origin)
            }
            cursor = NSMaxRange(word.range)
        }
        if cursor < string.length {
            draw(layout, NSRange(location: cursor, length: string.length - cursor), origin)
        }

        for word in pixelated {
            let glyphs = layout.manager.glyphRange(
                forCharacterRange: word.range, actualCharacterRange: nil)
            let frag = layout.manager.lineFragmentRect(
                forGlyphAt: glyphs.location, effectiveRange: nil)
            let at = layout.manager.location(forGlyphAt: glyphs.location)
            dissolve(
                word.grid,
                at: NSPoint(x: origin.x + frag.minX + at.x, y: origin.y + frag.minY + at.y),
                colour: word.colour, doubt: word.doubt)
        }
    }

    private func draw(_ layout: Layout, _ chars: NSRange, _ origin: NSPoint) {
        guard chars.length > 0 else { return }
        let glyphs = layout.manager.glyphRange(forCharacterRange: chars, actualCharacterRange: nil)
        guard glyphs.length > 0 else { return }
        layout.manager.drawGlyphs(forGlyphRange: glyphs, at: origin)
    }

    /// Draw one word as cells of ink instead of letters.
    ///
    /// A cell the stroke fills stays put and only the fringe erodes, so the word
    /// frays at its edges rather than thinning out from the middle. That
    /// distinction is the whole effect: a word you can still read but can see
    /// the model is unsure of, not a word that has been redacted.
    ///
    /// Cell alpha tracks coverage almost linearly for the same reason. Rounding
    /// every half-covered cell up to solid is what a pixelate filter does, and
    /// it lays down more ink than the letters it replaced — the word comes out
    /// heavier than its neighbours and draws the eye as an error rather than as
    /// a doubt.
    private func dissolve(_ grid: Grid, at baseline: NSPoint, colour: NSColor, doubt: CGFloat) {
        let top = baseline.y - grid.ascent
        // Darkened toward the deep sage, and further the less certain the word.
        // Both halves matter: the blend keeps the cells green rather than
        // letting a low alpha wash them out to grey, and the alpha is what
        // actually seats them below the line of text they sit in.
        let pen = colour.blended(withFraction: 0.45 + 0.30 * doubt, of: cSageDeep) ?? colour
        let alpha = colour.alphaComponent * (0.82 - 0.22 * doubt)
        let salt = grid.cols &* 31 &+ grid.rows

        func rect(_ i: Int) -> NSRect {
            NSRect(
                x: baseline.x + CGFloat(i % grid.cols) * grid.cell,
                y: top + CGFloat(i / grid.cols) * grid.cell,
                width: grid.cell, height: grid.cell)
        }

        /// Cells with enough ink left to be worth drawing, once the fringe has
        /// been eaten into. The interior is exempt: a solid cell survives any
        /// amount of doubt, which is what holds the word's skeleton together.
        // How hard the fringe is eaten into, eased off for narrow words. A long
        // word can lose a third of its edge and still be the word it was; "it"
        // is four cells across and the same bite leaves a mark rather than a
        // word. Short words are also where the model's mistakes cluster, so
        // this is the one place legibility has to win over the signal.
        let bite = 0.55 * doubt * min(1, CGFloat(grid.cols) / 9)

        var kept: [Int] = []
        for i in grid.coverage.indices {
            let ink = grid.coverage[i]
            guard ink > 0.12 else { continue }
            if ink >= 0.55 || ink * (1 - bite * TranscriptView.jitter(i, salt)) > 0.24 {
                kept.append(i)
            }
        }

        // A soft wash under the cells, as one path so overlaps don't stack into
        // a dark edge. This is the blur in the effect — what stops a grid of
        // squares reading as a QR code and makes it read as a word out of focus.
        let halo = NSBezierPath()
        for i in kept { halo.appendRect(rect(i).insetBy(dx: -0.7, dy: -0.7)) }
        pen.withAlphaComponent(alpha * (0.05 + 0.07 * doubt)).setFill()
        halo.fill()

        for i in kept {
            pen.withAlphaComponent(alpha * min(1, grid.coverage[i] * 1.15)).setFill()
            rect(i).fill()
        }
    }
}

// MARK: - Controller

final class Overlay {
    let panel: NSPanel
    let blur: NSVisualEffectView
    let pill: PillView
    var timer: Timer?
    let height: CGFloat = 40

    /// The live transcript, floating just above the pill.
    let transcript = TranscriptView(frame: .zero)
    /// Frosted glass behind it.
    ///
    /// The first version painted a translucent colour straight onto the panel,
    /// which is not the same thing at all: whatever was on screen behind it —
    /// a paragraph of someone else's text — showed through and tangled with the
    /// transcript. The pill solved this with an `NSVisualEffectView` on day one;
    /// this simply did not have one.
    let transcriptBlur = NSVisualEffectView(frame: .zero)
    let transcriptPanel: NSPanel
    /// Gap between the two surfaces. Enough that they read as separate things.
    private let transcriptGap: CGFloat = 10

    init() {
        pill = PillView(frame: NSRect(x: 0, y: 0, width: 200, height: height))
        let w = pill.contentWidth(.recording)
        let rect = NSRect(x: 0, y: 0, width: w, height: height)
        pill.frame = rect
        pill.autoresizingMask = [.width, .height]

        // Frosted glass, exactly the original CSS backdrop-filter intent.
        blur = NSVisualEffectView(frame: rect)
        blur.material = .hudWindow
        blur.blendingMode = .behindWindow
        blur.state = .active
        blur.wantsLayer = true
        blur.layer?.cornerRadius = height / 2
        blur.layer?.masksToBounds = true
        blur.layer?.borderColor = cBorder.cgColor
        blur.layer?.borderWidth = 1
        // Capsule mask with cap insets: height is fixed, so the middle stretches
        // and one mask serves every width.
        blur.maskImage = Overlay.capsuleMask(height)
        blur.addSubview(pill)

        panel = NSPanel(
            contentRect: rect, styleMask: [.nonactivatingPanel, .borderless],
            backing: .buffered, defer: false)
        panel.isOpaque = false
        panel.backgroundColor = .clear
        panel.hasShadow = true
        panel.level = .statusBar
        panel.collectionBehavior = [.canJoinAllSpaces, .fullScreenAuxiliary, .stationary]
        panel.hidesOnDeactivate = false
        panel.ignoresMouseEvents = true
        panel.contentView = blur
        panel.alphaValue = 0
        panel.orderFrontRegardless()

        // Its own panel so the pill keeps its shape and its capsule mask. Same
        // collection behaviour, so it follows onto full-screen Spaces too.
        transcriptPanel = NSPanel(
            contentRect: NSRect(x: 0, y: 0, width: TranscriptView.maxWidth, height: 40),
            styleMask: [.nonactivatingPanel, .borderless],
            backing: .buffered, defer: false)
        transcriptPanel.isOpaque = false
        transcriptPanel.backgroundColor = .clear
        transcriptPanel.hasShadow = true
        transcriptPanel.level = .statusBar
        transcriptPanel.collectionBehavior = [
            .canJoinAllSpaces, .fullScreenAuxiliary, .stationary,
        ]
        transcriptPanel.hidesOnDeactivate = false
        transcriptPanel.ignoresMouseEvents = true

        transcriptBlur.material = .hudWindow
        transcriptBlur.blendingMode = .behindWindow
        transcriptBlur.state = .active
        transcriptBlur.wantsLayer = true
        // A plain radius rather than the pill's capsule mask: this panel changes
        // height as lines wrap, and cap insets assume a fixed one.
        transcriptBlur.layer?.cornerRadius = 12
        transcriptBlur.layer?.masksToBounds = true
        transcriptBlur.layer?.borderColor = cBorder.cgColor
        transcriptBlur.layer?.borderWidth = 1
        transcript.autoresizingMask = [.width, .height]
        transcriptBlur.addSubview(transcript)
        transcriptPanel.contentView = transcriptBlur
        transcriptPanel.alphaValue = 0
        transcriptPanel.orderFrontRegardless()
    }

    /// Show the words heard so far, growing the panel to fit and keeping it
    /// sitting just above the pill.
    ///
    /// Ignored unless the pill is up: a preview landing after key-up would
    /// otherwise pop this back open as everything was fading out.
    func setTranscript(_ list: [Heard]) {
        guard panel.alphaValue > 0.5, pill.mode == .recording, !list.isEmpty else {
            if list.isEmpty { hideTranscript() }
            return
        }

        transcript.setWords(list)
        let size = transcript.fittingSize()
        guard size.height > 0 else { return }

        let centerX = panel.frame.midX
        let y = panel.frame.maxY + transcriptGap
        let frame = NSRect(
            x: centerX - size.width / 2, y: y, width: size.width, height: size.height)

        let firstShow = transcriptPanel.alphaValue < 0.5
        if firstShow {
            // Starts a touch low and rises into place, the same entrance the
            // pill makes — it should feel like the same object arriving, not a
            // second window appearing.
            transcriptPanel.setFrame(frame.offsetBy(dx: 0, dy: -8), display: false)
            transcriptBlur.frame = NSRect(origin: .zero, size: size)
            transcript.frame = NSRect(origin: .zero, size: size)
            transcriptPanel.alphaValue = 0
            transcriptPanel.orderFrontRegardless()
            NSAnimationContext.runAnimationGroup { ctx in
                ctx.duration = 0.34
                ctx.timingFunction = CAMediaTimingFunction(controlPoints: 0.16, 1.2, 0.3, 1)
                transcriptPanel.animator().alphaValue = 1
                transcriptPanel.animator().setFrame(frame, display: true)
            }
        } else {
            NSAnimationContext.runAnimationGroup { ctx in
                ctx.duration = 0.26
                // The same whisker of overshoot the pill settles with, so the
                // two surfaces move in one motion language.
                ctx.timingFunction = CAMediaTimingFunction(controlPoints: 0.16, 1.2, 0.3, 1)
                transcriptPanel.animator().setFrame(frame, display: true)
            }
            transcriptBlur.frame = NSRect(origin: .zero, size: size)
            transcript.frame = NSRect(origin: .zero, size: size)
        }
        transcript.needsDisplay = true
    }

    func hideTranscript() {
        guard transcriptPanel.alphaValue > 0 else { return }
        NSAnimationContext.runAnimationGroup(
            { ctx in
                ctx.duration = 0.2
                ctx.timingFunction = CAMediaTimingFunction(name: .easeIn)
                transcriptPanel.animator().alphaValue = 0
            }, completionHandler: { [weak self] in self?.transcript.setWords([]) })
    }

    private static func capsuleMask(_ h: CGFloat) -> NSImage {
        let s = NSSize(width: h + 2, height: h)
        let img = NSImage(size: s)
        img.lockFocus()
        NSColor.black.setFill()
        NSBezierPath(roundedRect: NSRect(origin: .zero, size: s), xRadius: h / 2, yRadius: h / 2).fill()
        img.unlockFocus()
        img.capInsets = NSEdgeInsets(top: 0, left: h / 2, bottom: 0, right: h / 2)
        img.resizingMode = .stretch
        return img
    }

    private func activeScreen() -> NSScreen? {
        let mouse = NSEvent.mouseLocation
        return NSScreen.screens.first(where: { NSMouseInRect(mouse, $0.frame, false) })
            ?? NSScreen.main ?? NSScreen.screens.first
    }

    /// Resize the pill to `w`, keeping its horizontal centre fixed.
    private func setWidth(_ w: CGFloat, centerX: CGFloat, animated: Bool) {
        let frame = NSRect(x: centerX - w / 2, y: panel.frame.origin.y, width: w, height: height)
        if animated {
            NSAnimationContext.runAnimationGroup { ctx in
                ctx.duration = 0.3
                ctx.timingFunction = CAMediaTimingFunction(controlPoints: 0.3, 1.05, 0.3, 1)
                panel.animator().setFrame(frame, display: true)
            }
        } else {
            panel.setFrame(frame, display: true)
        }
    }

    func show(mode: PillView.Mode) {
        // Recording is the only state with a live transcript: once the key is
        // released the real text is moments away, so leaving a stale partial
        // hanging over "TRANSCRIBING" would just be showing worse words.
        if mode != .recording { hideTranscript() }
        let w = pill.contentWidth(mode)

        // Already up (recording → transcribing): swap content and glide the
        // width to fit the new state, keeping the centre put.
        if panel.alphaValue > 0.5 {
            let centerX = panel.frame.midX
            pill.setMode(mode)
            setWidth(w, centerX: centerX, animated: true)
            startTimer()
            return
        }

        guard let f = activeScreen()?.visibleFrame else { return }
        let centerX = f.midX
        let fy = f.origin.y + 110

        pill.setMode(mode)
        panel.setFrame(NSRect(x: centerX - w / 2, y: fy - 12, width: w, height: height), display: false)
        panel.alphaValue = 0
        panel.orderFrontRegardless()
        // Gentle ease-out with a whisker of overshoot — the pill settles up.
        NSAnimationContext.runAnimationGroup { ctx in
            ctx.duration = 0.36
            ctx.timingFunction = CAMediaTimingFunction(controlPoints: 0.16, 1.2, 0.3, 1)
            panel.animator().alphaValue = 1
            panel.animator().setFrameOrigin(NSPoint(x: centerX - w / 2, y: fy))
        }
        startTimer()
    }

    func hide() {
        hideTranscript()
        NSAnimationContext.runAnimationGroup(
            { ctx in
                ctx.duration = 0.24
                ctx.timingFunction = CAMediaTimingFunction(name: .easeIn)
                panel.animator().alphaValue = 0
            }, completionHandler: { [weak self] in self?.stopTimer() })
    }

    private func startTimer() {
        guard timer == nil else { return }
        let t = Timer(timeInterval: 1.0 / 60.0, repeats: true) { [weak self] _ in
            self?.pill.tick()
            // Only while something is actually arriving — a static panel should
            // not repaint sixty times a second.
            if self?.transcript.isAnimating == true { self?.transcript.needsDisplay = true }
        }
        RunLoop.main.add(t, forMode: .common)
        timer = t
    }

    private func stopTimer() {
        timer?.invalidate()
        timer = nil
    }
}

// MARK: - main

// Two jobs, one binary. `--pdf <path>` is a plain stdin-to-file filter (see
// pdf.swift) and must return before any of the AppKit setup below: an accessory
// NSApplication that never runs its loop would leave the exporter hanging.
// Sharing the binary keeps the bundle to one helper to sign and to remember.
if CommandLine.arguments.count >= 3, CommandLine.arguments[1] == "--pdf" {
    exit(renderPDF(outputPath: CommandLine.arguments[2]))
}

NSApplication.shared.setActivationPolicy(.accessory)
let overlay = Overlay()

DispatchQueue.global(qos: .userInitiated).async {
    while let line = readLine(strippingNewline: true) {
        let parts = line.split(separator: " ", maxSplits: 1).map(String.init)
        guard let cmd = parts.first else { continue }
        DispatchQueue.main.async {
            switch cmd {
            case "show": overlay.show(mode: .recording)
            case "transcribing": overlay.show(mode: .transcribing)
            case "level":
                if parts.count > 1, let v = Double(parts[1]) { overlay.pill.setLevel(CGFloat(v)) }
            case "text":
                overlay.setTranscript(parts.count > 1 ? parseHeard(parts[1]) : [])
            case "hide": overlay.hide()
            case "quit": NSApplication.shared.terminate(nil)
            default: break
            }
        }
    }
    DispatchQueue.main.async { NSApplication.shared.terminate(nil) }
}

NSApplication.shared.run()
