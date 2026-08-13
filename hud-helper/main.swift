// VoiceDumps meeting HUD — the floating surfaces for calls.
//
// A meeting happens in someone else's window. Every control for it therefore
// has to live above that window, or the feature costs a Cmd-Tab at exactly the
// moment nobody wants to look away from the call.
//
// Three surfaces, deliberately unalike, because they answer different questions:
//
//   * The **offer**, top right, where notifications live. It asks something,
//     spends a visible six seconds, and leaves. Behaving like a notification is
//     the point: it is one.
//   * The **pill**, low on the right. A vertical lozenge the size of a thumb:
//     the mark, and a waveform proving it is listening. It asks nothing, so it
//     takes almost no room.
//   * The **transcript**, revealed by hovering the pill. Words as they are
//     heard, uncertain ones drawn as pixels rather than guesses — the same
//     honesty the dictation overlay uses, because a live preview that looks as
//     confident as the final text is a lie about what it knows.
//
// Why a separate helper from the dictation overlay: this one takes clicks and
// answers back. The dictation pill is deliberately inert — `ignoresMouseEvents
// = true`, one-way over stdin — and threading a reverse channel plus hit
// testing through a component that currently cannot be wrong would put the
// app's most-used feature at risk to save a process. They share a look and a
// wire format, not a lifetime.
//
// Protocol — one command per line on stdin:
//
//   detected <app name>     offer to take notes on a call that just started
//   recording               show the pill
//   levels <you> <others>   two 0..1 meters, ten times a second
//   elapsed <seconds>       the clock, owned by the app so it survives a redraw
//   partial <payload>       live words, encoded `<digit0-9><word>` per field —
//                           the digit is confidence×9, exactly as the dictation
//                           overlay already receives them
//   finishing               transcribing both sides; no longer stoppable
//   progress <n> <stage>    how far through that it is, 0..1 plus a label
//   hide                    take the surfaces away
//   quit
//
// …and one word per line back on stdout, when the user presses something:
//
//   take-notes / dismiss / stop

import Cocoa
import QuartzCore

// MARK: - Palette

private let cBorder = NSColor(calibratedRed: 0.72, green: 0.83, blue: 0.64, alpha: 0.22)
private let cSage = NSColor(calibratedRed: 0.72, green: 0.83, blue: 0.64, alpha: 1)
private let cSageDim = NSColor(calibratedRed: 0.56, green: 0.69, blue: 0.49, alpha: 1)
private let cSageDeep = NSColor(calibratedRed: 0.40, green: 0.50, blue: 0.34, alpha: 1)
private let cInk = NSColor(calibratedWhite: 0.95, alpha: 1)
private let cFaint = NSColor(calibratedWhite: 0.66, alpha: 1)
private let cAmber = NSColor(calibratedRed: 0.85, green: 0.60, blue: 0.28, alpha: 1)

/// Painted inside the frosted panel, under everything else.
///
/// Frosted glass alone is not a background: over a dark desktop it reads as a
/// dark card, and over a white one it turns pale grey and takes the light text
/// with it. A committed dark scrim means the card is the same card everywhere,
/// and the type on it always has something to sit on.
private let cScrim = NSColor(calibratedWhite: 0.07, alpha: 0.74)

private let microFont = NSFont.monospacedSystemFont(ofSize: 9, weight: .medium)
private let bodyFont = NSFont.systemFont(ofSize: 12, weight: .regular)
private let readingFont = NSFont.systemFont(ofSize: 13, weight: .regular)
private let dataFont = NSFont.monospacedDigitSystemFont(ofSize: 11, weight: .medium)

/// The brand mark — a 3×3 pixel cluster with two cells knocked out.
private let brandCells: [Bool] = [true, true, false, true, true, true, false, true, true]

private func send(_ word: String) {
    FileHandle.standardOutput.write(Data("\(word)\n".utf8))
}

// MARK: - Shared drawing

private func label(_ text: String, _ font: NSFont, _ colour: NSColor, at point: NSPoint) {
    (text as NSString).draw(at: point, withAttributes: [.font: font, .foregroundColor: colour])
}

private func textWidth(_ text: String, _ font: NSFont) -> CGFloat {
    (text as NSString).size(withAttributes: [.font: font]).width
}

/// Shorten to fit, with an ellipsis. Used so the offer's sentence can never run
/// underneath its button — which is exactly what it did before there was a
/// width to respect.
private func truncate(_ text: String, _ font: NSFont, to limit: CGFloat) -> String {
    guard textWidth(text, font) > limit else { return text }
    var result = text
    while !result.isEmpty, textWidth(result + "…", font) > limit {
        result.removeLast()
    }
    return result + "…"
}

private func drawBrand(at origin: NSPoint, cell: CGFloat, gap: CGFloat, colour: NSColor) {
    colour.setFill()
    for (index, lit) in brandCells.enumerated() where lit {
        let row = CGFloat(index / 3)
        let column = CGFloat(index % 3)
        NSRect(
            x: origin.x + column * (cell + gap),
            // Drawn top-down in a bottom-up coordinate space, so the mark reads
            // the same way it does everywhere else in the app.
            y: origin.y + (2 - row) * (cell + gap),
            width: cell, height: cell
        ).fill()
    }
}

private struct Hit {
    let rect: NSRect
    let action: () -> Void
}

/// A word as it was heard, with how sure the model was of it.
private struct Heard {
    let word: String
    let confidence: CGFloat
}

/// `<digit0-9><word>` per space-separated field — the encoding the dictation
/// overlay already speaks, reused verbatim so there is one format to reason
/// about rather than two that drift.
private func parseHeard(_ payload: String) -> [Heard] {
    payload.split(separator: " ").compactMap { field in
        guard field.count > 1, let digit = field.first?.wholeNumberValue,
            (0...9).contains(digit)
        else { return nil }
        return Heard(word: String(field.dropFirst()), confidence: CGFloat(digit) / 9.0)
    }
}

private class HitTestingView: NSView {
    var hits: [Hit] = []
    var hovered: Int? = nil

    override var isFlipped: Bool { false }

    override func mouseDown(with event: NSEvent) {
        let point = convert(event.locationInWindow, from: nil)
        for hit in hits where hit.rect.contains(point) {
            hit.action()
            return
        }
    }

    override func mouseMoved(with event: NSEvent) {
        let point = convert(event.locationInWindow, from: nil)
        let was = hovered
        hovered = hits.firstIndex { $0.rect.contains(point) }
        if was != hovered { needsDisplay = true }
    }

    override func updateTrackingAreas() {
        super.updateTrackingAreas()
        trackingAreas.forEach(removeTrackingArea)
        addTrackingArea(
            NSTrackingArea(
                rect: bounds, options: [.mouseMoved, .mouseEnteredAndExited, .activeAlways],
                owner: self, userInfo: nil))
    }

    func scrim(radius: CGFloat) {
        cScrim.setFill()
        NSBezierPath(roundedRect: bounds, xRadius: radius, yRadius: radius).fill()
    }
}

// MARK: - The offer

private final class OfferView: HitTestingView {
    var appName = "Another app"
    /// 0 → just appeared, 1 → leaving. Drawn as the line crossing the bottom.
    var progress: CGFloat = 0

    private let padding: CGFloat = 10
    private let buttonHeight: CGFloat = 24

    private func buttonWidth(_ text: String) -> CGFloat {
        textWidth(text, microFont) + padding * 2
    }

    override func draw(_ dirtyRect: NSRect) {
        hits.removeAll()
        scrim(radius: 13)

        let left: CGFloat = 15
        // One button, not two. The card already dismisses itself — a decline
        // button next to a countdown asks people to make a decision the card is
        // about to make for them, and doing nothing was always the way out.
        let takeWidth = buttonWidth("TAKE NOTES")
        let buttonX = bounds.width - 15 - takeWidth
        // The sentence gets everything to the left of the button and not a
        // pixel more; before this it simply ran underneath it.
        let textLimit = buttonX - left - 14

        drawBrand(at: NSPoint(x: left, y: bounds.height - 26), cell: 3, gap: 1.5, colour: cSage)
        label(
            "MEETING DETECTED", microFont, cSage,
            at: NSPoint(x: left + 20, y: bounds.height - 25))
        label(
            truncate("\(appName) is using your microphone", bodyFont, to: textLimit),
            bodyFont, cInk, at: NSPoint(x: left, y: bounds.height - 47))

        let rect = NSRect(
            x: buttonX, y: bounds.height / 2 - buttonHeight / 2 - 3,
            width: takeWidth, height: buttonHeight)
        let isHovered = hovered == 0
        (isHovered ? cSage : cSageDim).setFill()
        NSBezierPath(roundedRect: rect, xRadius: 5, yRadius: 5).fill()
        label(
            "TAKE NOTES", microFont, NSColor(calibratedWhite: 0.06, alpha: 1),
            at: NSPoint(x: rect.minX + padding, y: rect.midY - microFont.capHeight))
        hits.append(Hit(rect: rect, action: { send("take-notes") }))

        // A card that vanishes with no warning feels like a glitch; one that
        // visibly spends its time reads as a decision you were offered.
        cSageDeep.setFill()
        NSRect(x: 0, y: 0, width: bounds.width * min(max(progress, 0), 1), height: 2).fill()
    }
}

// MARK: - The pill

private final class PillView: HitTestingView {
    var finishing = false
    /// Set when the far side of the call is not arriving. Drawn on the pill
    /// rather than only in the hover panel, because a warning nobody can see
    /// without hovering is a warning nobody sees during a meeting.
    var warning = false
    /// How far through saving the meeting, 0 to 1. `shown` follows it.
    var progress: CGFloat = 0
    /// What is actually drawn, easing toward `progress`.
    ///
    /// Stages arrive in jumps — 0.1, then 0.5 when a whole track finishes —
    /// and a column that teleports reads as a glitch rather than as work being
    /// done. Following the number instead of printing it is most of the
    /// difference between a progress indicator and a status light.
    private var shown: CGFloat = 0
    /// Continuous, for the slow breath on the leading cell.
    private var breath: CGFloat = 0
    /// Bars settle to rest when the call ends rather than vanishing, so the
    /// waveform becomes the column instead of being replaced by it.
    private var settle: CGFloat = 0

    /// One frame of the finishing animation. Returns whether to keep going.
    func tick() {
        shown += (progress - shown) * 0.12
        breath += 0.055
        if settle < 1 { settle = min(1, settle + 0.06) }
        needsDisplay = true
    }

    func resetFinishing() {
        shown = 0
        breath = 0
        settle = 0
    }
    private var history: [CGFloat] = Array(repeating: 0, count: 7)

    /// The loudest thing heard lately, decaying slowly.
    ///
    /// The level that arrives is an absolute one, mapped for the app's wide
    /// meter where a bar's *position* is the reading. On something this size
    /// that mapping is nearly flat: measured across ordinary speech it moved
    /// the bars about six pixels, which is why the pill looked dead. Measuring
    /// each level against the recent peak instead spends the whole height on
    /// the range actually in use, and roughly doubles the swing.
    ///
    /// Starts at what ordinary speech peaks at rather than at nothing, so the
    /// first words of a meeting are scaled sensibly instead of pinning the bars
    /// while the reference catches up. It adapts down for a quiet speaker.
    private var peak: CGFloat = 0.75

    /// Below this, treat it as room tone rather than speech.
    ///
    /// Without a gate the auto-ranging turns a silent room into a full
    /// waveform, since the loudest thing in it is still the loudest thing.
    private let gate: CGFloat = 0.30

    /// A little room above the recent peak, so the loudest syllable is a tall
    /// bar rather than a clipped one. Without it every new maximum normalises
    /// to exactly full height and runs of speech flatten out at the ceiling —
    /// which is the same complaint, one level up.
    private let headroom: CGFloat = 1.05

    func push(you: CGFloat, others: CGFloat) {
        let raw = max(you, others)

        // Measured against the peak as it stood *before* this sample, for the
        // reason above.
        let ceiling = max(peak * headroom, gate + 0.15)
        let above = raw - gate
        let normalised = above <= 0 ? 0 : min(1, above / (ceiling - gate))

        // Rises instantly, falls over a few seconds — so a sudden loud moment
        // is not lost, and a quiet passage after it re-expands rather than
        // staying squashed against an old maximum.
        peak = raw > peak ? raw : peak * 0.992 + raw * 0.008

        history.removeFirst()
        history.append(normalised)
        needsDisplay = true
    }

    func reset() {
        history = Array(repeating: 0, count: history.count)
        peak = 0.75
        needsDisplay = true
    }

    override func draw(_ dirtyRect: NSRect) {
        hits.removeAll()
        scrim(radius: bounds.width / 2)

        // Mark on top, waveform beneath — the whole vocabulary of the thing.
        // No clock and no stop button up here: both live in the transcript that
        // hovering reveals, and putting them on a lozenge this size would mean
        // shrinking the two marks that actually say what it is.
        let cell: CGFloat = 3.5
        let gap: CGFloat = 2
        let markWidth = cell * 3 + gap * 2
        drawBrand(
            at: NSPoint(x: bounds.midX - markWidth / 2, y: bounds.height - 30),
            cell: cell, gap: gap,
            colour: warning ? cAmber : (finishing ? cSageDeep : cSage))

        if finishing {
            // The waveform does not disappear, it comes to rest. Every bar
            // eases to the same short mark and the column of progress rises
            // through them, so the pill is one object changing what it is
            // doing rather than two widgets swapping places. `settle` runs
            // 0 to 1 over about a third of a second, which is long enough to
            // read as deliberate and short enough not to be a performance.
            let cells = 7
            let size: CGFloat = 4
            let step: CGFloat = 6
            let bottom = 30 - (CGFloat(cells) * step - (step - size)) / 2
            let filled = shown * CGFloat(cells)

            for i in 0..<cells {
                let y = bottom + CGFloat(i) * step
                // Where this bar was when the call ended, easing to the rest
                // height. The waveform is drawn centred, so it collapses
                // inward rather than dropping.
                let was = max(3, min(max(history[i], 0), 1) * 26)
                let height = was + (size - was) * settle
                let rect = NSRect(
                    x: bounds.midX - size / 2, y: y + (size - height) / 2,
                    width: size, height: height)

                // Done, working, or waiting. The leading cell breathes on a
                // slow sine rather than blinking: at this size a hard on/off
                // is a fault light, and the machine is fine — it is busy.
                let done = CGFloat(i) + 1 <= filled
                let leading = !done && CGFloat(i) < filled + 1
                if done {
                    cSage.setFill()
                } else if leading {
                    let pulse = 0.5 + 0.5 * sin(breath)
                    cSageDim.withAlphaComponent(0.35 + 0.5 * pulse).setFill()
                } else {
                    cSageDeep.setFill()
                }
                NSBezierPath(roundedRect: rect, xRadius: size / 2, yRadius: size / 2).fill()
            }
            return
        }

        let barWidth: CGFloat = 3
        let barGap: CGFloat = 3
        let total = CGFloat(history.count) * barWidth + CGFloat(history.count - 1) * barGap
        var x = bounds.midX - total / 2
        let centre: CGFloat = 30
        for level in history {
            let clamped = min(max(level, 0), 1)
            // Never zero: a flat row reads as a dead connection, a floor of a
            // few pixels reads as "listening, nobody talking". The ceiling is
            // what the auto-ranging above earns — the range is now spent on
            // speech rather than on the gap between speech and silence.
            let height = max(3, clamped * 26)
            cSageDim.setFill()
            NSBezierPath(
                roundedRect: NSRect(
                    x: x, y: centre - height / 2, width: barWidth, height: height),
                xRadius: barWidth / 2, yRadius: barWidth / 2
            ).fill()
            x += barWidth + barGap
        }
    }
}

// MARK: - The transcript

private final class TranscriptView: HitTestingView {
    var words: [Heard] = []
    var elapsed: Int = 0
    var finishing = false
    /// Non-empty when the far side of the call is not being received.
    var warning = ""
    /// What the app is doing to the recording, and how far through it is.
    var stage = "Stopping"
    var progress: CGFloat = 0

    /// The line of trust, and how far below it a word has fallen.
    ///
    /// Ported from the dictation overlay rather than reinvented — these are its
    /// numbers, measured there against real speech: marking everything below
    /// 0.80 catches every word the model got wrong, at the price of texturing
    /// roughly one correct word in nine. Erring that way is deliberate, because
    /// a false mark costs a glance and a missed error is the thing the preview
    /// is being distrusted for.
    private static func doubt(_ confidence: CGFloat) -> CGFloat {
        guard confidence < 0.80 else { return 0 }
        return min(1, (0.80 - confidence) / 0.55)
    }

    /// Stable per-cell jitter, so a word's fringe erodes unevenly.
    ///
    /// A flat threshold takes the same amount off every edge, which just makes
    /// the word lighter — it reads as a thinner font rather than as something
    /// coming apart. A hash rather than a random number because the panel
    /// repaints and a word that boils is unreadable in a way a word that has
    /// broken up is not.
    private static func jitter(_ i: Int, _ salt: Int) -> CGFloat {
        var h = UInt32(truncatingIfNeeded: (i &+ 1) &* 2_654_435_761 &+ salt &* 40_503)
        h ^= h >> 15
        h = h &* 2_246_822_519
        h ^= h >> 13
        return CGFloat(h % 1024) / 1024
    }

    private func clock(_ seconds: Int) -> String {
        let m = seconds / 60
        let s = seconds % 60
        return m >= 60
            ? String(format: "%d:%02d:%02d", m / 60, m % 60, s)
            : String(format: "%d:%02d", m, s)
    }

    override func draw(_ dirtyRect: NSRect) {
        hits.removeAll()
        scrim(radius: 14)

        let left: CGFloat = 16
        let right = bounds.width - 16

        // Header: what this is, how long it has been, and the way out. A
        // warning takes the status line over, because "recording" is the
        // reassuring word and it is the one thing that is not quite true.
        let heading = warning.isEmpty ? (finishing ? "FINISHING" : "RECORDING") : warning
        let headingColour = warning.isEmpty ? cSage : cAmber
        drawBrand(
            at: NSPoint(x: left, y: bounds.height - 26), cell: 3, gap: 1.5,
            colour: headingColour)
        label(
            heading, microFont, headingColour,
            at: NSPoint(x: left + 20, y: bounds.height - 25))
        let time = clock(elapsed)
        label(
            time, dataFont, cFaint,
            at: NSPoint(x: left + 20 + textWidth("RECORDING", microFont) + 10,
                        y: bounds.height - 26))

        if !finishing {
            let stopWidth = textWidth("STOP", microFont) + 20
            let stopRect = NSRect(
                x: right - stopWidth, y: bounds.height - 31, width: stopWidth, height: 22)
            let isHovered = hovered == 0
            (isHovered ? cSage : cBorder).setStroke()
            NSBezierPath(roundedRect: stopRect.insetBy(dx: 0.5, dy: 0.5), xRadius: 5, yRadius: 5)
                .stroke()
            label(
                "STOP", microFont, isHovered ? cInk : cFaint,
                at: NSPoint(x: stopRect.minX + 10, y: stopRect.midY - microFont.capHeight))
            hits.append(Hit(rect: stopRect, action: { send("stop") }))
        }

        cBorder.setFill()
        NSRect(x: left, y: bounds.height - 40, width: bounds.width - 32, height: 1).fill()

        if finishing {
            drawFinishing(
                in: NSRect(
                    x: left, y: 14, width: bounds.width - 32, height: bounds.height - 56))
            return
        }

        drawWords(
            in: NSRect(
                x: left, y: 14, width: bounds.width - 32, height: bounds.height - 56))
    }

    /// What happens after the call: two transcriptions, a mixdown and a save.
    ///
    /// Worth showing rather than leaving to a spinner, because this is the one
    /// part of a meeting that takes minutes and produces nothing to look at. A
    /// person who cannot tell the difference between "working" and "hung" will
    /// assume hung, and the words they are waiting for are already most of the
    /// way here.
    private func drawFinishing(in rect: NSRect) {
        label(
            stage.uppercased(), microFont, cSage,
            at: NSPoint(x: rect.minX, y: rect.maxY - 14))

        // Segmented, like every other progress bar in the app.
        let cells = 32
        let gap: CGFloat = 2
        let cellWidth = (rect.width - gap * CGFloat(cells - 1)) / CGFloat(cells)
        let filled = Int((progress.clamped() * CGFloat(cells)).rounded())
        for i in 0..<cells {
            (i < filled ? cSageDim : cBorder).setFill()
            NSRect(
                x: rect.minX + CGFloat(i) * (cellWidth + gap), y: rect.maxY - 34,
                width: cellWidth, height: 4
            ).fill()
        }

        label(
            "Both sides are transcribed separately, then interleaved.", bodyFont, cFaint,
            at: NSPoint(x: rect.minX, y: rect.maxY - 58))

        // The words heard so far stay on screen underneath. They are about to
        // be replaced by a better reading of the same audio, but they are also
        // the proof that the thing being processed is the conversation that
        // just happened.
        if !words.isEmpty {
            drawWords(in: NSRect(x: rect.minX, y: rect.minY, width: rect.width,
                                 height: rect.height - 74))
        }
    }

    /// Lay the words out bottom-up, newest last, wrapping as they go.
    ///
    /// Built backwards from the end because the interesting words are the
    /// recent ones: when there are more than fit, the opening of the meeting is
    /// what should fall off the top.
    private func drawWords(in rect: NSRect) {
        guard !words.isEmpty else {
            label(
                finishing ? "Transcribing both sides…" : "Listening…", bodyFont, cFaint,
                at: NSPoint(x: rect.minX, y: rect.maxY - 16))
            return
        }

        let lineHeight: CGFloat = 20
        let spaceWidth = textWidth(" ", readingFont)

        // First pass: wrap into lines.
        var lines: [[Heard]] = [[]]
        var used: CGFloat = 0
        for heard in words {
            let width = wordWidth(heard)
            if used > 0, used + spaceWidth + width > rect.width {
                lines.append([])
                used = 0
            }
            lines[lines.count - 1].append(heard)
            used += (used > 0 ? spaceWidth : 0) + width
        }

        // Second pass: draw the last N that fit, oldest at the top.
        let capacity = max(1, Int(rect.height / lineHeight))
        let shown = lines.suffix(capacity)
        var y = rect.maxY - lineHeight
        for line in shown {
            var x = rect.minX
            for heard in line {
                draw(heard, at: NSPoint(x: x, y: y))
                x += wordWidth(heard) + spaceWidth
            }
            y -= lineHeight
        }
    }

    /// An uncertain word occupies exactly the width its letters would have,
    /// so the paragraph does not reflow when a word firms up.
    private func wordWidth(_ heard: Heard) -> CGFloat {
        textWidth(heard.word, readingFont)
    }

    private func draw(_ heard: Heard, at point: NSPoint) {
        let doubt = Self.doubt(heard.confidence)
        if doubt <= 0 {
            label(heard.word, readingFont, cInk, at: point)
            return
        }
        guard let grid = Self.grid(for: heard.word) else {
            label(heard.word, readingFont, cFaint, at: point)
            return
        }
        dissolve(grid, at: point, colour: cInk, doubt: doubt)
    }

    /// Draw one word as cells of ink instead of letters.
    ///
    /// A cell the stroke fills stays put and only the fringe erodes, so the word
    /// frays at its edges rather than thinning out from the middle. That
    /// distinction is the whole effect: a word you can still read but can see
    /// the model is unsure of, not a word that has been redacted — which is
    /// exactly what the first attempt here produced, and why this is now the
    /// dictation overlay's routine rather than an approximation of it.
    private func dissolve(_ grid: Grid, at origin: NSPoint, colour: NSColor, doubt: CGFloat) {
        // Darkened toward the deep sage, and further the less certain the word.
        // Both halves matter: the blend keeps the cells green rather than
        // letting a low alpha wash them out to grey, and the alpha is what
        // seats them below the line of text they sit in.
        let pen = colour.blended(withFraction: 0.45 + 0.30 * doubt, of: cSageDeep) ?? colour
        let alpha = colour.alphaComponent * (0.82 - 0.22 * doubt)
        let salt = grid.cols &* 31 &+ grid.rows
        let boxHeight = CGFloat(grid.rows) * grid.cell

        func rect(_ i: Int) -> NSRect {
            NSRect(
                x: origin.x + CGFloat(i % grid.cols) * grid.cell,
                // The grid is indexed from its top row down; this view is
                // bottom-up, so the row index counts back from the box top.
                y: origin.y + grid.descent + boxHeight - CGFloat(i / grid.cols + 1) * grid.cell,
                width: grid.cell, height: grid.cell)
        }

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
            // The interior is exempt: a solid cell survives any amount of
            // doubt, which is what holds the word's skeleton together.
            if ink >= 0.55 || ink * (1 - bite * Self.jitter(i, salt)) > 0.24 {
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

        // Cell alpha tracks coverage almost linearly. Rounding every
        // half-covered cell up to solid is what a pixelate filter does, and it
        // lays down more ink than the letters it replaced — the word comes out
        // heavier than its neighbours and draws the eye as an error rather than
        // as a doubt.
        for i in kept {
            pen.withAlphaComponent(alpha * min(1, grid.coverage[i] * 1.15)).setFill()
            rect(i).fill()
        }
    }
}

/// One word broken into cells of ink coverage.
private struct Grid {
    let cols: Int
    let rows: Int
    let cell: CGFloat
    /// How far the box extends below the drawing origin — the font's descender,
    /// which is where an unflipped `NSString.draw(at:)` puts the bottom.
    let descent: CGFloat
    let coverage: [CGFloat]
}

extension TranscriptView {
    /// The side of one cell, in points.
    ///
    /// Fixed, and deliberately small: at reading size an x-height is a little
    /// over four cells, which is enough to keep a word's shape and not enough
    /// to pretend the letters are still there. Coarser cells do not degrade a
    /// word, they delete it.
    static let cellSide: CGFloat = 2

    /// Cell grids, keyed by the word they were measured from.
    ///
    /// A word's grid depends only on its letters and the size of a cell — not
    /// on where it ended up — so it survives every rewrap and every repaint.
    /// Without this the panel would rasterise the same handful of words on
    /// every frame.
    private static var grids: [String: Grid] = [:]

    /// Break one word into cells of ink coverage.
    ///
    /// Rendered once in white and read back averaged over squares. Coverage
    /// rather than a threshold, so a stroke that only clips a cell still
    /// registers there — that is what lets the shape erode at its edges instead
    /// of dropping out in whole blocks.
    static func grid(for word: String) -> Grid? {
        if let cached = grids[word] { return cached }

        let width = textWidth(word, readingFont)
        let height = readingFont.ascender - readingFont.descender
        guard width >= 1, height >= 1 else { return nil }

        // Sampled at twice the point size, so a cell averages four device
        // pixels rather than being a single one rounded off.
        let scale: CGFloat = 2
        let wide = Int(ceil(width * scale))
        let high = Int(ceil(height * scale))
        guard
            let rep = NSBitmapImageRep(
                bitmapDataPlanes: nil, pixelsWide: wide, pixelsHigh: high,
                bitsPerSample: 8, samplesPerPixel: 4, hasAlpha: true, isPlanar: false,
                colorSpaceName: .deviceRGB, bytesPerRow: 0, bitsPerPixel: 0)
        else { return nil }

        NSGraphicsContext.saveGraphicsState()
        NSGraphicsContext.current = NSGraphicsContext(bitmapImageRep: rep)
        NSColor.clear.setFill()
        NSRect(x: 0, y: 0, width: CGFloat(wide), height: CGFloat(high)).fill()
        let transform = NSAffineTransform()
        transform.scaleX(by: scale, yBy: scale)
        transform.concat()
        // White, so every sample read back is coverage and nothing else; the
        // colour the word is drawn in is applied per cell later.
        (word as NSString).draw(
            at: .zero,
            withAttributes: [.font: readingFont, .foregroundColor: NSColor.white])
        NSGraphicsContext.restoreGraphicsState()

        guard rep.bitsPerSample == 8, rep.samplesPerPixel == 4, let data = rep.bitmapData
        else { return nil }

        let step = max(1, Int((cellSide * scale).rounded()))
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

        let grid = Grid(
            cols: cols, rows: rows, cell: cellSide, descent: readingFont.descender,
            coverage: coverage)
        // Bounded: a long meeting is a few thousand distinct words, and holding
        // a grid for each is cheap, but not unbounded-cheap.
        if grids.count > 2_000 { grids.removeAll(keepingCapacity: true) }
        grids[word] = grid
        return grid
    }
}

extension CGFloat {
    fileprivate func clamped() -> CGFloat { Swift.max(0, Swift.min(1, self)) }
}

// MARK: - Panels

/// The silhouette of a panel, as a mask for its frosted backing.
///
/// Exact-size rather than stretched with cap insets: none of these panels
/// resize, so there is nothing for the insets to solve.
private func shapeMask(size: NSSize, radius: CGFloat) -> NSImage {
    let image = NSImage(size: size)
    image.lockFocus()
    NSColor.black.setFill()
    NSBezierPath(
        roundedRect: NSRect(origin: .zero, size: size), xRadius: radius, yRadius: radius
    ).fill()
    image.unlockFocus()
    return image
}

private func makePanel(size: NSSize, radius: CGFloat, content: NSView) -> NSPanel {
    let rect = NSRect(origin: .zero, size: size)
    content.frame = rect
    content.autoresizingMask = [.width, .height]

    let blur = NSVisualEffectView(frame: rect)
    blur.material = .hudWindow
    blur.blendingMode = .behindWindow
    blur.state = .active
    // Pinned dark rather than following the system: these sit over other apps,
    // and a card that changes character with the desktop behind it is a card
    // with no character.
    blur.appearance = NSAppearance(named: .darkAqua)
    blur.wantsLayer = true
    // A mask image, not a corner radius.
    //
    // `NSVisualEffectView` draws its blur through a private backdrop layer that
    // `masksToBounds` on the view's own layer does not clip — so the rounded
    // card gets drawn, and the full rectangle of frosted glass gets drawn
    // behind it. On a dark desktop nobody notices; on a light one it is a white
    // box with a pill floating in it. `maskImage` is the supported way to shape
    // one of these, and it is what the dictation overlay has always used.
    blur.maskImage = shapeMask(size: size, radius: radius)
    blur.layer?.borderColor = cBorder.cgColor
    blur.layer?.borderWidth = 1
    blur.layer?.cornerRadius = radius
    blur.addSubview(content)

    let panel = NSPanel(
        contentRect: rect, styleMask: [.nonactivatingPanel, .borderless],
        backing: .buffered, defer: false)
    panel.isOpaque = false
    panel.backgroundColor = .clear
    panel.hasShadow = true
    panel.level = .statusBar
    panel.collectionBehavior = [.canJoinAllSpaces, .fullScreenAuxiliary, .stationary]
    panel.hidesOnDeactivate = false
    // Inert until something is actually shown in it. See `slideIn`.
    //
    // Unlike the dictation pill, these cards are meant to be pressed, and the
    // obvious way to write that is to set this once, here, and forget it. That
    // was the bug: every panel is ordered onto the screen at launch and simply
    // faded to nothing, and an `NSPanel` at alpha 0 is still there as far as
    // hit-testing is concerned. Three invisible rectangles — 360×74, 52×92 and
    // 380×240 — sat stacked at the bottom-left corner from the moment the app
    // started, swallowing every click that landed in them. No meeting, no
    // window, nothing on screen, and a dead patch of desktop.
    //
    // Alpha and mouse events now move together, so a panel can only take a
    // click while it is something a person can see.
    panel.ignoresMouseEvents = true
    panel.appearance = NSAppearance(named: .darkAqua)
    panel.contentView = blur
    panel.alphaValue = 0
    panel.orderFrontRegardless()
    return panel
}

private final class HUD {
    private let offerPanel: NSPanel
    private let pillPanel: NSPanel
    private let transcriptPanel: NSPanel
    private let offer = OfferView()
    private let pill = PillView()
    private let transcript = TranscriptView()

    private var offerTimer: Timer?
    private var offerShownAt: Date?
    private var hoverTimer: Timer?
    private var marchTimer: Timer?

    /// How long an unanswered offer stays up. Long enough to notice and read,
    /// short enough that ignoring it is a real answer rather than a chore.
    private let offerLifetime: TimeInterval = 6

    private let offerSize = NSSize(width: 360, height: 74)
    private let pillSize = NSSize(width: 52, height: 92)
    private let transcriptSize = NSSize(width: 380, height: 240)

    init() {
        offerPanel = makePanel(size: offerSize, radius: 13, content: offer)
        pillPanel = makePanel(size: pillSize, radius: pillSize.width / 2, content: pill)
        transcriptPanel = makePanel(size: transcriptSize, radius: 14, content: transcript)

        // The transcript follows the pointer, not a click: hovering something
        // to see more of it is the gesture this borrows, and asking for a click
        // first would make a glance cost a decision.
        hoverTimer = Timer.scheduledTimer(withTimeInterval: 0.15, repeats: true) {
            [weak self] _ in
            self?.updateHover()
        }
    }

    // MARK: Placement

    /// Where macOS puts notifications, because this is one.
    private func offerFrame(offscreen: Bool) -> NSRect {
        guard let visible = NSScreen.main?.visibleFrame else { return .zero }
        let x = visible.maxX - offerSize.width - 14
        let y = visible.maxY - offerSize.height - 10
        return NSRect(
            x: offscreen ? x + 26 : x, y: y, width: offerSize.width, height: offerSize.height)
    }

    /// Low on the right: out of the way of a video call's own controls, which
    /// cluster along the bottom centre, and far from the offer so the two are
    /// never mistaken for each other.
    private func pillFrame(offscreen: Bool) -> NSRect {
        guard let visible = NSScreen.main?.visibleFrame else { return .zero }
        let x = visible.maxX - pillSize.width - 22
        let y = visible.minY + visible.height * 0.20
        return NSRect(
            x: offscreen ? x + 26 : x, y: y, width: pillSize.width, height: pillSize.height)
    }

    /// Immediately left of the pill, bottom-aligned with it.
    private func transcriptFrame() -> NSRect {
        let anchor = pillFrame(offscreen: false)
        return NSRect(
            x: anchor.minX - transcriptSize.width - 10, y: anchor.minY,
            width: transcriptSize.width, height: transcriptSize.height)
    }

    /// Bring a card in, and let it start taking clicks.
    ///
    /// The transcript readout is not slid — it fades, in `fadeTranscript`, which
    /// has to do the same thing to `ignoresMouseEvents` for the same reason.
    /// This used to say the transcript needed no clicks because nothing in it
    /// was pressable. That was wrong: it draws the STOP button, which is the
    /// only way to end a meeting from the HUD, and the claim cost that button
    /// for a whole release. Any panel that becomes visible must become clickable
    /// in the same breath.
    private func slideIn(_ panel: NSPanel, to frame: NSRect, from offscreen: NSRect) {
        panel.setFrame(offscreen, display: false)
        panel.alphaValue = 0
        panel.ignoresMouseEvents = false
        NSAnimationContext.runAnimationGroup { context in
            context.duration = 0.26
            context.timingFunction = CAMediaTimingFunction(name: .easeOut)
            panel.animator().setFrame(frame, display: true)
            panel.animator().alphaValue = 1
        }
    }

    private func slideOut(_ panel: NSPanel, to offscreen: NSRect) {
        // Before the guard, not after: a panel that is already invisible is
        // exactly the one that must not be holding on to the pointer, and the
        // guard's early return is the path a launched-but-unused app takes.
        //
        // "Offscreen" here is twenty-six points to the side, which is a slide,
        // not an exit — a 380-point card parked that way is still 354 points of
        // screen. Alpha is what hides these; this is what makes them harmless.
        panel.ignoresMouseEvents = true
        guard panel.alphaValue > 0.01 else { return }
        NSAnimationContext.runAnimationGroup { context in
            context.duration = 0.2
            context.timingFunction = CAMediaTimingFunction(name: .easeIn)
            panel.animator().setFrame(offscreen, display: true)
            panel.animator().alphaValue = 0
        }
    }

    // MARK: Hover

    /// Polled rather than driven by enter/exit events, because the pointer
    /// crosses a real gap between the pill and the transcript. Tracking areas
    /// would fire an exit in that gap and close the very thing being reached
    /// for; asking "is the pointer over either of them" cannot flicker.
    private func updateHover() {
        guard pillPanel.alphaValue > 0.5 else {
            if transcriptPanel.alphaValue > 0.01 { fadeTranscript(visible: false) }
            return
        }
        let mouse = NSEvent.mouseLocation

        // What counts as "hovering" depends on whether the panel is already
        // open, and getting this wrong is very noticeable.
        //
        // Closed, only the pill opens it. The union of pill and transcript was
        // one rectangle reaching from the transcript's far edge to the pill —
        // roughly 380x240 points of screen that belonged to whatever app was
        // underneath, silently intercepting the pointer and popping a panel
        // over someone's work.
        //
        // Open, the region grows to cover the transcript and the gap between
        // them, because the pointer has to cross that gap to reach the panel
        // and a region that stopped at the pill would close the very thing
        // being reached for. Polled rather than driven by enter/exit events for
        // the same reason: an exit event in the gap cannot be told apart from
        // an exit that meant it.
        let open = transcriptPanel.alphaValue > 0.5
        let hot =
            open
            ? pillFrame(offscreen: false).union(transcriptFrame()).insetBy(dx: -6, dy: -6)
            : pillFrame(offscreen: false).insetBy(dx: -6, dy: -6)
        fadeTranscript(visible: hot.contains(mouse))
    }

    private func fadeTranscript(visible: Bool) {
        // Before the guard on both paths, exactly as in `slideIn`/`slideOut`:
        // hidden means inert, shown means clickable, and the STOP button inside
        // depends on the second half of that.
        //
        // Setting it while hidden is not enough on its own — the panel is only
        // ever *reached* while it is showing, because the pointer has to be over
        // it to keep it open. But the guard below returns early whenever the
        // alpha is already where it is going, and a card left inert on the way
        // in would be a card whose button does nothing.
        transcriptPanel.ignoresMouseEvents = !visible
        let wanted: CGFloat = visible ? 1 : 0
        guard abs(transcriptPanel.alphaValue - wanted) > 0.01 else { return }
        if visible {
            transcriptPanel.setFrame(transcriptFrame(), display: false)
        }
        NSAnimationContext.runAnimationGroup { context in
            context.duration = 0.16
            transcriptPanel.animator().alphaValue = wanted
        }
    }

    // MARK: Commands

    func showOffer(_ appName: String) {
        offer.appName = appName
        offer.progress = 0
        offer.needsDisplay = true
        slideIn(offerPanel, to: offerFrame(offscreen: false), from: offerFrame(offscreen: true))

        offerShownAt = Date()
        offerTimer?.invalidate()
        offerTimer = Timer.scheduledTimer(withTimeInterval: 1.0 / 30, repeats: true) {
            [weak self] timer in
            guard let self, let shown = self.offerShownAt else {
                timer.invalidate()
                return
            }
            let elapsed = Date().timeIntervalSince(shown)
            self.offer.progress = CGFloat(min(1, elapsed / self.offerLifetime))
            self.offer.needsDisplay = true
            if elapsed >= self.offerLifetime {
                timer.invalidate()
                self.hideOffer()
                send("dismiss")
            }
        }
    }

    func hideOffer() {
        offerTimer?.invalidate()
        offerTimer = nil
        offerShownAt = nil
        slideOut(offerPanel, to: offerFrame(offscreen: true))
    }

    func showRecorder() {
        hideOffer()
        pill.finishing = false
        transcript.finishing = false
        // A new meeting starts clean; last call's warning is not this one's.
        pill.warning = false
        transcript.warning = ""
        transcript.words = []
        transcript.elapsed = 0
        pill.reset()
        slideIn(pillPanel, to: pillFrame(offscreen: false), from: pillFrame(offscreen: true))
    }

    /// Something is wrong with the capture but the meeting is still running.
    ///
    /// Not an alert and not a dialog: a call is exactly the wrong moment to put
    /// a modal in front of someone. The mark goes amber where they can see it
    /// without moving, and the words are there when they look.
    func warn(_ text: String) {
        pill.warning = !text.isEmpty
        transcript.warning = text
        pill.needsDisplay = true
        transcript.needsDisplay = true
    }

    func showFinishing() {
        pill.finishing = true
        transcript.finishing = true
        transcript.stage = "Stopping"
        transcript.progress = 0
        pill.needsDisplay = true
        transcript.needsDisplay = true
        if pillPanel.alphaValue < 0.5 {
            slideIn(pillPanel, to: pillFrame(offscreen: false), from: pillFrame(offscreen: true))
        }
        pill.resetFinishing()
        marchTimer?.invalidate()
        // Thirty times a second, because this drives an eased fill and a slow
        // pulse rather than a three-state blink. The old 0.4s tick was enough
        // to say "something is happening" and nothing more.
        marchTimer = Timer.scheduledTimer(withTimeInterval: 1.0 / 30.0, repeats: true) {
            [weak self] _ in
            guard let self, self.pill.finishing else { return }
            self.pill.tick()
        }
    }

    func progress(fraction: CGFloat, stage: String) {
        transcript.progress = fraction
        if !stage.isEmpty { transcript.stage = stage }
        transcript.needsDisplay = true
        // The pill shows it too, and now has to: with the panel no longer
        // opening itself, this is the only thing on screen once a call ends.
        pill.progress = fraction
        pill.needsDisplay = true
    }

    func hideAll() {
        marchTimer?.invalidate()
        marchTimer = nil
        hideOffer()
        fadeTranscript(visible: false)
        slideOut(pillPanel, to: pillFrame(offscreen: true))
        // The finishing look is held until the pill is actually gone. Clearing
        // it here used to hand the last fifth of a second back to the waveform,
        // so a meeting ended on a flash of the thing that says "listening" —
        // arriving precisely when it had stopped.
        DispatchQueue.main.asyncAfter(deadline: .now() + 0.25) { [weak self] in
            guard let self, self.pillPanel.alphaValue < 0.01 else { return }
            self.pill.finishing = false
            self.transcript.finishing = false
        }
    }

    func levels(you: CGFloat, others: CGFloat) {
        guard pillPanel.alphaValue > 0.5, !pill.finishing else { return }
        pill.push(you: you, others: others)
    }

    func elapsed(_ seconds: Int) {
        transcript.elapsed = seconds
        if transcriptPanel.alphaValue > 0.01 { transcript.needsDisplay = true }
    }

    /// The whole visible tail, not an append.
    ///
    /// The second pass re-reads chunks the first pass already showed, and it may
    /// hear a different number of words in them — so a correction cannot be a
    /// patch at an offset. Sending the tail every time makes replacement the
    /// only operation, and the payload is a few hundred bytes.
    func partial(_ payload: String) {
        transcript.words = parseHeard(payload)
        if transcriptPanel.alphaValue > 0.01 { transcript.needsDisplay = true }
    }
}

// MARK: - main

NSApplication.shared.setActivationPolicy(.accessory)

private let hud = HUD()

Thread.detachNewThread {
    while let line = readLine(strippingNewline: true) {
        let parts = line.split(separator: " ", maxSplits: 1).map(String.init)
        guard let command = parts.first else { continue }
        let argument = parts.count > 1 ? parts[1] : ""

        DispatchQueue.main.async {
            switch command {
            case "detected":
                hud.showOffer(argument.isEmpty ? "Another app" : argument)
            case "recording":
                hud.showRecorder()
            case "finishing":
                hud.showFinishing()
            case "levels":
                let numbers = argument.split(separator: " ").compactMap { Double($0) }
                if numbers.count == 2 {
                    hud.levels(you: CGFloat(numbers[0]), others: CGFloat(numbers[1]))
                }
            case "elapsed":
                hud.elapsed(Int(argument) ?? 0)
            case "partial":
                hud.partial(argument)
            case "progress":
                // `<fraction> <stage words…>`
                let parts = argument.split(separator: " ", maxSplits: 1).map(String.init)
                hud.progress(
                    fraction: CGFloat(Double(parts.first ?? "") ?? 0),
                    stage: parts.count > 1 ? parts[1] : "")
            case "warn":
                hud.warn(argument)
            case "hide":
                hud.hideAll()
            case "quit":
                NSApplication.shared.terminate(nil)
            default:
                break
            }
        }
    }
    // The app closed our stdin: it is gone, and floating cards belonging to
    // nothing would sit on screen forever.
    DispatchQueue.main.async { NSApplication.shared.terminate(nil) }
}

NSApplication.shared.run()
