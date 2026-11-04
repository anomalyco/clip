// The menu bar icons at 2× (18 pt): a template of the app icon's brackets and dot (macOS
// tints it for light and dark menu bars), and a red dot for recording.
// Usage: swift scripts/menubar-icons.swift assets
import AppKit

let dir = CommandLine.arguments.dropFirst().first ?? "assets"
func draw(_ name: String, _ body: (CGFloat) -> Void) {
    let px = 36
    let rep = NSBitmapImageRep(bitmapDataPlanes: nil, pixelsWide: px, pixelsHigh: px, bitsPerSample: 8, samplesPerPixel: 4,
                               hasAlpha: true, isPlanar: false, colorSpaceName: .deviceRGB, bytesPerRow: 0, bitsPerPixel: 0)!
    NSGraphicsContext.saveGraphicsState()
    NSGraphicsContext.current = NSGraphicsContext(bitmapImageRep: rep)
    body(CGFloat(px))
    NSGraphicsContext.restoreGraphicsState()
    try! rep.representation(using: .png, properties: [:])!.write(to: URL(fileURLWithPath: "\(dir)/\(name)"))
}
func brackets(_ s: CGFloat) {
    let inset: CGFloat = 4, arm: CGFloat = 9, corner: CGFloat = 3
    NSColor.black.setStroke()
    for (sx, sy) in [(-1.0, -1.0), (1.0, -1.0), (-1.0, 1.0), (1.0, 1.0)] as [(CGFloat, CGFloat)] {
        let x = sx < 0 ? inset : s - inset, y = sy < 0 ? inset : s - inset
        let p = NSBezierPath()
        p.move(to: NSPoint(x: x, y: y - sy * arm))
        p.line(to: NSPoint(x: x, y: y - sy * corner))
        p.curve(to: NSPoint(x: x - sx * corner, y: y), controlPoint1: NSPoint(x: x, y: y - sy * corner * 0.45),
                controlPoint2: NSPoint(x: x - sx * corner * 0.45, y: y))
        p.line(to: NSPoint(x: x - sx * arm, y: y))
        p.lineWidth = 3.4
        p.lineCapStyle = .round
        p.stroke()
    }
}
draw("menubar-idle@2x.png") { s in
    brackets(s)
    NSColor.black.setFill()
    NSBezierPath(ovalIn: NSRect(x: s / 2 - 6, y: s / 2 - 6, width: 12, height: 12)).fill()
}
draw("menubar-recording@2x.png") { s in
    NSColor(srgbRed: 1, green: 0.27, blue: 0.23, alpha: 1).setFill()
    NSBezierPath(ovalIn: NSRect(x: s / 2 - 8, y: s / 2 - 8, width: 16, height: 16)).fill()
}
