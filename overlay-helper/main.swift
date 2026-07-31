// VoiceDumps dictation overlay — a standalone accessory process.
//
// Why a separate process at all: a Tauri/tao webview window cannot enter another
// app's active full-screen Space, no matter its window level or collection
// behaviour (measured exhaustively). A native accessory NSPanel can. So the pill
// the user sees while dictating lives here, and the main app drives it over a
// pipe.
//
// Design: "Field Notes" — forest & sage, squares not circles, the
// pixel-cluster brand mark, and stepped/mechanical motion. The one concession is
// the rounded frosted container: a hard rectangle floating over another app's UI
// reads as a system error, a soft frosted pill reads as a quiet status HUD.
//
// Protocol — one command per line on stdin:
//   show / transcribing / level <0..1> / hide / quit

import Cocoa
import QuartzCore

// MARK: - Palette (Field Notes §3)

private let cForestTint = NSColor(calibratedRed: 0.09, green: 0.12, blue: 0.07, alpha: 0.55)
private let cBorder = NSColor(calibratedRed: 0.72, green: 0.83, blue: 0.64, alpha: 0.20)
private let cSage = NSColor(calibratedRed: 0.72, green: 0.83, blue: 0.64, alpha: 1)
private let cSageDim = NSColor(calibratedRed: 0.56, green: 0.69, blue: 0.49, alpha: 1)
private let cSageBright = NSColor(calibratedRed: 0.82, green: 0.90, blue: 0.76, alpha: 1)

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
    static let maxLines = 4
    static let padX: CGFloat = 16
    static let padY: CGFloat = 11

    private(set) var text: String = ""
    /// How much of `text` has finished arriving. Everything past this is the
    /// newest chunk, and is what gets animated in.
    private var settled: Int = 0
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
        !text.isEmpty
            && CFAbsoluteTimeGetCurrent() - arrivedAt < revealDuration + settleDuration
    }

    override var isFlipped: Bool { true }

    static var attrs: [NSAttributedString.Key: Any] {
        let para = NSMutableParagraphStyle()
        para.lineBreakMode = .byWordWrapping
        para.lineSpacing = 3
        return [
            .font: NSFont.systemFont(ofSize: 13, weight: .regular),
            // Base is the settled weight; the newest run overrides it.
            .foregroundColor: cSageDim.withAlphaComponent(0.66),
            .paragraphStyle: para,
        ]
    }

    /// The tail of the transcript that fits in `maxLines`.
    ///
    /// The tail, because while you are still talking the words that just
    /// arrived are the ones worth seeing; the opening has already done its job
    /// of telling you it heard you correctly.
    func visible() -> String {
        if text.isEmpty { return "" }
        var words = text.split(separator: " ").map(String.init)
        while !words.isEmpty {
            let candidate = words.joined(separator: " ")
            if TranscriptView.linesFor(candidate) <= TranscriptView.maxLines {
                return candidate == text ? candidate : "… " + candidate
            }
            words.removeFirst()
        }
        return ""
    }

    private static func linesFor(_ s: String) -> Int {
        let h = measure(s).height
        let line = NSAttributedString(string: "Ag", attributes: attrs).size().height
        return max(1, Int((h / max(line, 1)).rounded()))
    }

    static func measure(_ s: String) -> NSSize {
        guard !s.isEmpty else { return .zero }
        let box = NSAttributedString(string: s, attributes: attrs).boundingRect(
            with: NSSize(width: maxWidth, height: .greatestFiniteMagnitude),
            options: [.usesLineFragmentOrigin, .usesFontLeading])
        return NSSize(width: ceil(box.width), height: ceil(box.height))
    }

    /// The panel size this text wants, or `.zero` when there is nothing to show.
    func fittingSize() -> NSSize {
        let s = visible()
        if s.isEmpty { return .zero }
        let m = TranscriptView.measure(s)
        return NSSize(
            width: min(m.width, TranscriptView.maxWidth) + TranscriptView.padX * 2,
            height: m.height + TranscriptView.padY * 2)
    }

    func setText(_ s: String) {
        // The preview appends, so the previous text is normally a prefix of the
        // new one — anything beyond it is the chunk that just landed. A
        // wholesale change (a reset, or whisper revising itself) reveals the
        // lot rather than pretending part of it was already on screen.
        if !text.isEmpty && s.hasPrefix(text) && s.count > text.count {
            settled = text.count
        } else {
            settled = 0
        }
        text = s
        arrivedAt = CFAbsoluteTimeGetCurrent()
        needsDisplay = true
    }

    /// The visible text, with the newest chunk part-way through arriving.
    ///
    /// Built as one attributed string rather than drawn in two passes: the
    /// paragraph wraps, so drawing the tail separately would need its own line
    /// layout and the two halves would not line up.
    private func rendered() -> NSAttributedString {
        let s = visible()
        let out = NSMutableAttributedString(string: s, attributes: TranscriptView.attrs)
        guard !s.isEmpty else { return out }

        // How much of the *visible* string is new. `visible()` may have dropped
        // words off the front and added an ellipsis, so this is counted from
        // the end, where the arrivals are.
        let fresh = min(max(text.count - settled, 0), s.count)
        guard fresh > 0 else { return out }

        let age = CFAbsoluteTimeGetCurrent() - arrivedAt
        let range = NSRange(location: s.count - fresh, length: fresh)

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

        let colour =
            freshColor.blended(withFraction: t, of: restColor) ?? freshColor
        out.addAttribute(
            .foregroundColor, value: colour.withAlphaComponent(alpha), range: range)
        if rise != 0 {
            out.addAttribute(.baselineOffset, value: rise, range: range)
        }
        return out
    }

    override func draw(_ dirtyRect: NSRect) {
        let b = bounds
        // The frosted view behind this is the ground; all that is added here is
        // a wash of forest so sage text keeps its contrast over a bright
        // background as well as a dark one.
        cForestTint.withAlphaComponent(0.46).setFill()
        b.fill()

        let s = rendered()
        guard s.length > 0 else { return }
        s.draw(
            with: NSRect(
                x: TranscriptView.padX, y: TranscriptView.padY,
                width: b.width - TranscriptView.padX * 2,
                height: b.height - TranscriptView.padY * 2),
            options: [.usesLineFragmentOrigin, .usesFontLeading])
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
    func setTranscript(_ s: String) {
        guard panel.alphaValue > 0.5, pill.mode == .recording, !s.isEmpty else {
            if s.isEmpty { hideTranscript() }
            return
        }

        transcript.setText(s)
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
            }, completionHandler: { [weak self] in self?.transcript.setText("") })
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
                overlay.setTranscript(parts.count > 1 ? parts[1] : "")
            case "hide": overlay.hide()
            case "quit": NSApplication.shared.terminate(nil)
            default: break
            }
        }
    }
    DispatchQueue.main.async { NSApplication.shared.terminate(nil) }
}

NSApplication.shared.run()
