#!/usr/bin/env swift
// Generates the Vibe Factory app icon set into VibeFactory/Assets.xcassets/AppIcon.appiconset:
// a night-blue squircle, a factory with a saw-tooth roof and lit windows, and
// "vibe" waves rising from its chimney. Drawn in code so it stays reproducible.
// Usage (from apps/macos/VibeFactory): ./Scripts/make-app-icon.swift
import AppKit

let scriptDir = URL(fileURLWithPath: CommandLine.arguments[0]).deletingLastPathComponent()
let assets = scriptDir.deletingLastPathComponent()
    .appendingPathComponent("VibeFactory/Assets.xcassets/AppIcon.appiconset")
guard FileManager.default.fileExists(atPath: assets.path) else {
    FileHandle.standardError.write(Data("could not locate \(assets.path)\n".utf8))
    exit(1)
}

func rgb(_ hex: UInt32, _ alpha: CGFloat = 1) -> CGColor {
    CGColor(srgbRed: CGFloat((hex >> 16) & 0xFF) / 255, green: CGFloat((hex >> 8) & 0xFF) / 255,
            blue: CGFloat(hex & 0xFF) / 255, alpha: alpha)
}

/// Draws in a 1024-unit space scaled to `size` pixels (y up, origin bottom left).
func render(_ size: Int) -> Data {
    // A bitmap of exactly size×size pixels (lockFocus would render at 2× on Retina).
    let rep = NSBitmapImageRep(
        bitmapDataPlanes: nil, pixelsWide: size, pixelsHigh: size,
        bitsPerSample: 8, samplesPerPixel: 4, hasAlpha: true, isPlanar: false,
        colorSpaceName: .deviceRGB, bytesPerRow: 0, bitsPerPixel: 0)!
    let ctx = NSGraphicsContext(bitmapImageRep: rep)!.cgContext
    ctx.scaleBy(x: CGFloat(size) / 1024, y: CGFloat(size) / 1024)
    let space = CGColorSpace(name: CGColorSpace.sRGB)!
    let small = size <= 64   // fewer, bolder details for the 16–64 px sizes

    // Squircle on Apple's macOS grid: 824 units with a 100-unit margin, soft drop shadow.
    let body = CGRect(x: 100, y: 100, width: 824, height: 824)
    let squircle = CGPath(roundedRect: body, cornerWidth: 185, cornerHeight: 185, transform: nil)
    ctx.saveGState()
    ctx.setShadow(offset: CGSize(width: 0, height: -12), blur: 28, color: rgb(0x000000, 0.35))
    ctx.addPath(squircle); ctx.setFillColor(rgb(0x141A3A)); ctx.fillPath()
    ctx.restoreGState()

    ctx.saveGState()
    ctx.addPath(squircle); ctx.clip()
    // Night sky: indigo at the top to deep blue at the bottom.
    let sky = CGGradient(colorsSpace: space, colors: [rgb(0x3B2A8C), rgb(0x16204A)] as CFArray,
                         locations: [0, 1])!
    ctx.drawLinearGradient(sky, start: CGPoint(x: 512, y: 924), end: CGPoint(x: 512, y: 100), options: [])
    // Warm glow behind the chimney.
    let glow = CGGradient(colorsSpace: space, colors: [rgb(0xFF6FA8, 0.45), rgb(0xFF6FA8, 0)] as CFArray,
                          locations: [0, 1])!
    ctx.drawRadialGradient(glow, startCenter: CGPoint(x: 700, y: 700), startRadius: 0,
                           endCenter: CGPoint(x: 700, y: 700), endRadius: 340, options: [])

    // Vibe waves rising from the chimney: sine strokes, orange → pink → violet.
    let waves = small ? 2 : 3
    for i in 0..<waves {
        let baseY = 690 + CGFloat(i) * (small ? 110 : 78)
        let x0: CGFloat = 600, x1: CGFloat = 830
        let wave = CGMutablePath()
        wave.move(to: CGPoint(x: x0, y: baseY))
        var x = x0
        while x <= x1 {
            let t = (x - x0) / (x1 - x0)
            wave.addLine(to: CGPoint(x: x, y: baseY + sin(t * .pi * 2) * (small ? 30 : 22)))
            x += 4
        }
        ctx.saveGState()
        ctx.addPath(wave)
        ctx.setLineWidth(small ? 54 : 34); ctx.setLineCap(.round); ctx.setLineJoin(.round)
        ctx.replacePathWithStrokedPath(); ctx.clip()
        let colors = [[rgb(0xFFB547), rgb(0xFF7A59)], [rgb(0xFF7A59), rgb(0xFF4F9A)],
                      [rgb(0xFF4F9A), rgb(0xC45BFF)]][small ? i * 2 : i]
        let g = CGGradient(colorsSpace: space, colors: colors as CFArray, locations: [0, 1])!
        ctx.drawLinearGradient(g, start: CGPoint(x: x0, y: baseY), end: CGPoint(x: x1, y: baseY),
                               options: [.drawsBeforeStartLocation, .drawsAfterEndLocation])
        ctx.restoreGState()
    }

    // Factory: saw-tooth roof, a chimney, then a flat-roofed hall, on a ground line.
    let ground: CGFloat = 210, eave: CGFloat = 470
    let wall = rgb(0xF2F0FF)
    let factory = CGMutablePath()
    factory.move(to: CGPoint(x: 190, y: ground))
    factory.addLine(to: CGPoint(x: 190, y: eave))
    let teeth = 3, left: CGFloat = 190, right: CGFloat = 630
    let tooth = (right - left) / CGFloat(teeth)
    for k in 0..<teeth {
        let x = left + CGFloat(k) * tooth
        factory.addLine(to: CGPoint(x: x + tooth, y: eave + 110))   // sloped roof up
        if k < teeth - 1 {
            factory.addLine(to: CGPoint(x: x + tooth, y: eave))      // skylight down
        }
    }
    factory.addLine(to: CGPoint(x: 630, y: 610))                     // chimney, against the last tooth
    factory.addLine(to: CGPoint(x: 710, y: 610))
    factory.addLine(to: CGPoint(x: 710, y: eave))
    factory.addLine(to: CGPoint(x: 834, y: eave))                     // hall
    factory.addLine(to: CGPoint(x: 834, y: ground))
    factory.closeSubpath()
    ctx.addPath(factory)
    ctx.setFillColor(wall); ctx.fillPath()
    ctx.addPath(CGPath(roundedRect: CGRect(x: 614, y: 606, width: 112, height: 28),
                       cornerWidth: 8, cornerHeight: 8, transform: nil))  // chimney cap
    ctx.fillPath()
    // Ground line under the building.
    ctx.addPath(CGPath(roundedRect: CGRect(x: 160, y: ground - 34, width: 704, height: 22),
                       cornerWidth: 11, cornerHeight: 11, transform: nil))
    ctx.setFillColor(rgb(0xF2F0FF, 0.55)); ctx.fillPath()

    // Windows: dark panes with an amber prompt, like terminals at work.
    let rows: [CGFloat] = small ? [270] : [260, 355]
    let cols: [CGFloat] = small ? [230, 420, 620] : [230, 330, 430, 530, 640, 740]
    let w: CGFloat = small ? 130 : 64, h: CGFloat = small ? 120 : 58
    ctx.setFillColor(rgb(0x2A2466))
    for y in rows {
        for x in cols {
            ctx.addPath(CGPath(roundedRect: CGRect(x: x, y: y, width: w, height: h),
                               cornerWidth: 10, cornerHeight: 10, transform: nil))
        }
    }
    ctx.fillPath()
    if !small {
        ctx.setFillColor(rgb(0xFFB547))
        for y in rows {
            for x in cols { ctx.fill(CGRect(x: x + 13, y: y + 14, width: 24, height: 11)) }
        }
    }
    ctx.restoreGState()

    return rep.representation(using: .png, properties: [:])!
}

for size in [16, 32, 64, 128, 256, 512, 1024] {
    let url = assets.appendingPathComponent("icon_\(size).png")
    try! render(size).write(to: url)
    print("wrote \(url.lastPathComponent)")
}
print("✅ Vibe Factory icon set generated")
