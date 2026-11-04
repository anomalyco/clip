// The flat icon artwork, drawn in code: a red record dot with two quiet tap rings inside
// white viewfinder brackets, on a solid dark ground. No gloss, glow or gradients.
// Usage: swift scripts/icon-art.swift art.png   (then scripts/icon.swift for the macOS shape)
import AppKit

let out = CommandLine.arguments.dropFirst().first ?? "assets/clip-icon-flat-art.png"
let size = 1024.0
func rgb(_ hex: UInt32, _ a: CGFloat = 1) -> NSColor {
    NSColor(srgbRed: CGFloat((hex >> 16) & 0xff) / 255, green: CGFloat((hex >> 8) & 0xff) / 255, blue: CGFloat(hex & 0xff) / 255, alpha: a)
}
let rep = NSBitmapImageRep(bitmapDataPlanes: nil, pixelsWide: Int(size), pixelsHigh: Int(size), bitsPerSample: 8,
                           samplesPerPixel: 4, hasAlpha: true, isPlanar: false, colorSpaceName: .deviceRGB, bytesPerRow: 0, bitsPerPixel: 0)!
NSGraphicsContext.saveGraphicsState()
NSGraphicsContext.current = NSGraphicsContext(bitmapImageRep: rep)
let c = size / 2

rgb(0x1b1b21).setFill()
NSRect(x: 0, y: 0, width: size, height: size).fill()

// Tap rings, then the dot.
for (radius, alpha) in [(300.0, 0.16), (232.0, 0.3)] {
    let ring = NSBezierPath(ovalIn: NSRect(x: c - radius, y: c - radius, width: 2 * radius, height: 2 * radius))
    ring.lineWidth = 14
    rgb(0xff453a, alpha).setStroke()
    ring.stroke()
}
let dot = 158.0
rgb(0xff453a).setFill()
NSBezierPath(ovalIn: NSRect(x: c - dot, y: c - dot, width: 2 * dot, height: 2 * dot)).fill()

// Viewfinder brackets: an L in each corner with round caps and joins.
let inset = 176.0, arm = 178.0, corner = 46.0
rgb(0xf4f4f6).setStroke()
for (sx, sy) in [(-1.0, -1.0), (1.0, -1.0), (-1.0, 1.0), (1.0, 1.0)] {
    let x = sx < 0 ? inset : size - inset
    let y = sy < 0 ? inset : size - inset
    let p = NSBezierPath()
    p.move(to: NSPoint(x: x, y: y - sy * arm))
    p.line(to: NSPoint(x: x, y: y - sy * corner))
    p.curve(to: NSPoint(x: x - sx * corner, y: y), controlPoint1: NSPoint(x: x, y: y - sy * corner * 0.45),
            controlPoint2: NSPoint(x: x - sx * corner * 0.45, y: y))
    p.line(to: NSPoint(x: x - sx * arm, y: y))
    p.lineWidth = 58
    p.lineCapStyle = .round
    p.lineJoinStyle = .round
    p.stroke()
}
NSGraphicsContext.restoreGraphicsState()
try! rep.representation(using: .png, properties: [:])!.write(to: URL(fileURLWithPath: out))
