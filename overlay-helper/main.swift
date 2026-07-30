// VoiceDumps dictation overlay — a standalone accessory process.
//
// Why a separate process at all: a Tauri/tao webview window cannot enter another
// app's active full-screen Space, no matter its window level or collection
// behaviour (measured exhaustively). A native accessory NSPanel can. So the pill
// the user sees while dictating lives here, and the main app drives it over a
// pipe.
//
// Design: Field Notes "Field Notes" — forest & sage, squares not circles, the
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

// MARK: - Controller

final class Overlay {
    let panel: NSPanel
    let blur: NSVisualEffectView
    let pill: PillView
    var timer: Timer?
    let height: CGFloat = 40

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
        NSAnimationContext.runAnimationGroup(
            { ctx in
                ctx.duration = 0.24
                ctx.timingFunction = CAMediaTimingFunction(name: .easeIn)
                panel.animator().alphaValue = 0
            }, completionHandler: { [weak self] in self?.stopTimer() })
    }

    private func startTimer() {
        guard timer == nil else { return }
        let t = Timer(timeInterval: 1.0 / 60.0, repeats: true) { [weak self] _ in self?.pill.tick() }
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
            case "hide": overlay.hide()
            case "quit": NSApplication.shared.terminate(nil)
            default: break
            }
        }
    }
    DispatchQueue.main.async { NSApplication.shared.terminate(nil) }
}

NSApplication.shared.run()
