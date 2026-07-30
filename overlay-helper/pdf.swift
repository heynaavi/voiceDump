// Transcript -> PDF.
//
// This lives in the overlay helper's binary rather than one of its own. The
// helper is already built, bundled and signed; every extra executable in the
// bundle is another thing to wire into `bundle.resources` and another thing to
// forget, which has already cost this project a silently missing dictation
// overlay once. `main.swift` branches to `renderPDF` before it touches
// NSApplication, so in this mode the process is a plain CLI filter: JSON in on
// stdin, a PDF on disk, exit.
//
// CoreText rather than a PDF library because the typography is the point. The
// app's own Space Grotesk and JetBrains Mono are registered from the bundle, so
// the exported page is set in the same faces as the screen.
//
// The design follows Field Notes but is not a screenshot of it: paper is not a
// dark UI. Forest ink on white, one sage hairline, timestamps hung in the left
// margin the way a transcript wants them. No logo, no cover, no banner — the
// page is the transcript.

import AppKit
import CoreText
import Foundation

// MARK: - Page geometry (A4, points)

private let pageW: CGFloat = 595.276
private let pageH: CGFloat = 841.89
private let marginL: CGFloat = 56
private let marginR: CGFloat = 56
private let marginTop: CGFloat = 64
private let marginBottom: CGFloat = 56

/// Timestamps hang here, outside the text column, so the prose keeps one clean
/// left edge and the times stay scannable.
private let stampW: CGFloat = 40
private let stampGap: CGFloat = 14
private let textX = marginL + stampW + stampGap
private let textW = pageW - textX - marginR

private let bodySize: CGFloat = 10.5
private let bodyLine: CGFloat = 17.5
private let paraGap: CGFloat = 15

/// Never strand fewer than about three lines of a paragraph at the foot of a
/// page — a lone line under a heading is the classic ugly break.
private let minOrphanBlock = bodyLine * 3

// MARK: - Ink

private func ink(_ alpha: CGFloat) -> CGColor {
    // --color-ink #1b2015, the same forest the app sets text in.
    CGColor(red: 0.106, green: 0.125, blue: 0.082, alpha: alpha)
}
/// --color-sage-dim #8fb07c, used once, for the rule under the title.
private let sageDim = CGColor(red: 0.561, green: 0.690, blue: 0.486, alpha: 1)

// MARK: - Input

private struct Para {
    let stamp: String
    let text: String
}

private struct Doc {
    let title: String
    let meta: String
    let paragraphs: [Para]
    let sansPath: String?
    let monoPath: String?
}

private func parse(_ json: [String: Any]) -> Doc {
    let paras = (json["paragraphs"] as? [[String: Any]] ?? []).map {
        Para(stamp: $0["stamp"] as? String ?? "", text: $0["text"] as? String ?? "")
    }
    return Doc(
        title: json["title"] as? String ?? "Transcript",
        meta: json["meta"] as? String ?? "",
        paragraphs: paras.filter { !$0.text.isEmpty },
        sansPath: json["sans"] as? String,
        monoPath: json["mono"] as? String
    )
}

// MARK: - Fonts

/// Load a font file directly rather than by name.
///
/// Registering and then looking the family up by string is the usual recipe and
/// the usual bug: the PostScript name of a variable font is not what the CSS
/// calls it, and a miss falls back to Helvetica without saying so. Building the
/// descriptor from the file itself cannot miss.
private func fontFromFile(_ path: String?, size: CGFloat, weight: CGFloat?) -> CTFont? {
    guard let path, FileManager.default.fileExists(atPath: path) else { return nil }
    let url = URL(fileURLWithPath: path) as CFURL
    CTFontManagerRegisterFontsForURL(url, .process, nil)
    guard
        let descs = CTFontManagerCreateFontDescriptorsFromURL(url) as? [CTFontDescriptor],
        var desc = descs.first
    else { return nil }

    // Both faces are variable; pull the weight axis rather than letting AppKit
    // synthesise a fake bold, which smears at title sizes.
    if let weight {
        let wght = 0x77676874  // 'wght'
        let variation = [NSNumber(value: wght): NSNumber(value: Double(weight))]
        desc = CTFontDescriptorCreateCopyWithAttributes(
            desc, [kCTFontVariationAttribute: variation] as CFDictionary)
    }
    return CTFontCreateWithFontDescriptor(desc, size, nil)
}

private func fallback(_ size: CGFloat, mono: Bool, bold: Bool = false) -> CTFont {
    let f = mono
        ? NSFont.monospacedSystemFont(ofSize: size, weight: bold ? .semibold : .regular)
        : NSFont.systemFont(ofSize: size, weight: bold ? .semibold : .regular)
    return f as CTFont
}

// MARK: - Paragraph style

private func lineStyle(_ height: CGFloat) -> CTParagraphStyle {
    var minH = height
    var maxH = height
    return withUnsafeMutablePointer(to: &minH) { lo in
        withUnsafeMutablePointer(to: &maxH) { hi in
            let settings = [
                CTParagraphStyleSetting(
                    spec: .minimumLineHeight, valueSize: MemoryLayout<CGFloat>.size, value: lo),
                CTParagraphStyleSetting(
                    spec: .maximumLineHeight, valueSize: MemoryLayout<CGFloat>.size, value: hi),
            ]
            return CTParagraphStyleCreate(settings, settings.count)
        }
    }
}

// MARK: - Render

func renderPDF(outputPath: String) -> Int32 {
    let data = FileHandle.standardInput.readDataToEndOfFile()
    guard
        let raw = try? JSONSerialization.jsonObject(with: data),
        let obj = raw as? [String: Any]
    else {
        FileHandle.standardError.write(Data("pdf: could not read the transcript JSON\n".utf8))
        return 2
    }
    let doc = parse(obj)

    let sans = fontFromFile(doc.sansPath, size: bodySize, weight: nil) ?? fallback(bodySize, mono: false)
    let sansTitle = fontFromFile(doc.sansPath, size: 20, weight: 600) ?? fallback(20, mono: false, bold: true)
    let mono = { (s: CGFloat) in
        fontFromFile(doc.monoPath, size: s, weight: nil) ?? fallback(s, mono: true)
    }

    var box = CGRect(x: 0, y: 0, width: pageW, height: pageH)
    let info: [String: Any] = [
        kCGPDFContextTitle as String: doc.title,
        kCGPDFContextCreator as String: "VoiceDumps",
    ]
    guard
        let ctx = CGContext(
            URL(fileURLWithPath: outputPath) as CFURL, mediaBox: &box, info as CFDictionary)
    else {
        FileHandle.standardError.write(Data("pdf: could not create \(outputPath)\n".utf8))
        return 3
    }

    let bodyAttrs: [NSAttributedString.Key: Any] = [
        kCTFontAttributeName as NSAttributedString.Key: sans,
        kCTForegroundColorAttributeName as NSAttributedString.Key: ink(1),
        kCTParagraphStyleAttributeName as NSAttributedString.Key: lineStyle(bodyLine),
    ]

    var y: CGFloat = 0
    var pageNo = 0

    /// One line of type, drawn from a baseline. `rightEdge` right-aligns it.
    func draw(_ s: String, font: CTFont, color: CGColor, x: CGFloat, baseline: CGFloat,
              tracking: CGFloat = 0, rightEdge: CGFloat? = nil) {
        guard !s.isEmpty else { return }
        var attrs: [NSAttributedString.Key: Any] = [
            kCTFontAttributeName as NSAttributedString.Key: font,
            kCTForegroundColorAttributeName as NSAttributedString.Key: color,
        ]
        if tracking != 0 {
            attrs[kCTKernAttributeName as NSAttributedString.Key] = tracking
        }
        let line = CTLineCreateWithAttributedString(NSAttributedString(string: s, attributes: attrs))
        var startX = x
        if let rightEdge {
            let w = CTLineGetTypographicBounds(line, nil, nil, nil)
            startX = rightEdge - CGFloat(w)
        }
        ctx.textPosition = CGPoint(x: startX, y: baseline)
        CTLineDraw(line, ctx)
    }

    func endPage() {
        guard pageNo > 0 else { return }
        // The only furniture on the page. A transcript does not need a banner;
        // it needs to be findable once it is printed and on a desk.
        draw("\(pageNo)", font: mono(7), color: ink(0.32),
             x: 0, baseline: 34, rightEdge: pageW - marginR)
        ctx.endPDFPage()
    }

    func startPage() {
        endPage()
        ctx.beginPDFPage(nil)
        pageNo += 1
        y = pageH - marginTop
    }

    startPage()

    // -- masthead, first page only -------------------------------------------
    y -= CTFontGetAscent(sansTitle)
    draw(doc.title, font: sansTitle, color: ink(1), x: marginL, baseline: y)
    y -= CTFontGetDescent(sansTitle) + 16

    if !doc.meta.isEmpty {
        draw(doc.meta.uppercased(), font: mono(7.5), color: ink(0.45),
             x: marginL, baseline: y, tracking: 1.1)
        y -= 14
    }

    ctx.setFillColor(sageDim)
    ctx.fill(CGRect(x: marginL, y: y, width: pageW - marginL - marginR, height: 0.7))
    y -= 28

    // -- body -----------------------------------------------------------------
    for para in doc.paragraphs {
        let attr = NSAttributedString(string: para.text, attributes: bodyAttrs)
        let framesetter = CTFramesetterCreateWithAttributedString(attr)
        var start = 0
        var firstChunk = true

        while start < attr.length {
            if y - marginBottom < minOrphanBlock {
                startPage()
            }
            let availH = y - marginBottom

            // The frame is built at the origin and the context translated to it,
            // so the line origins CoreText hands back need no reinterpretation.
            ctx.saveGState()
            ctx.translateBy(x: textX, y: marginBottom)
            let path = CGPath(
                rect: CGRect(x: 0, y: 0, width: textW, height: availH), transform: nil)
            let frame = CTFramesetterCreateFrame(
                framesetter, CFRange(location: start, length: 0), path, nil)
            let visible = CTFrameGetVisibleStringRange(frame)

            if visible.length == 0 {
                ctx.restoreGState()
                startPage()
                continue
            }
            CTFrameDraw(frame, ctx)

            let lines = (CTFrameGetLines(frame) as? [CTLine]) ?? []
            var origins = [CGPoint](repeating: .zero, count: lines.count)
            CTFrameGetLineOrigins(frame, CFRange(location: 0, length: 0), &origins)
            ctx.restoreGState()

            // Hang the timestamp off the first line of the paragraph only — on a
            // continuation page the prose simply carries on.
            if firstChunk, let top = origins.first {
                draw(para.stamp, font: mono(7.5), color: ink(0.38),
                     x: 0, baseline: marginBottom + top.y, rightEdge: marginL + stampW)
                firstChunk = false
            }

            if let last = origins.last, let lastLine = lines.last {
                var descent: CGFloat = 0
                CTLineGetTypographicBounds(lastLine, nil, &descent, nil)
                y = marginBottom + last.y - descent
            } else {
                y -= availH
            }

            start += visible.length
            if start < attr.length {
                startPage()
            }
        }
        y -= paraGap
    }

    endPage()
    ctx.closePDF()
    return 0
}
