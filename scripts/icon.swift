// Turns square artwork into a macOS app icon: the 824 pt rounded-square body on Apple's
// 1024 pt grid (100 pt margin), a soft drop shadow, and a thin inner edge highlight.
// Usage: swift scripts/icon.swift art.png out-1024.png
import AppKit

let args = CommandLine.arguments
guard args.count == 3, let art = NSImage(contentsOfFile: args[1]) else {
    FileHandle.standardError.write("usage: icon.swift art.png out.png\n".data(using: .utf8)!)
    exit(2)
}
let size = 1024.0, inset = 100.0, body = size - 2 * inset, radius = body * 0.2237
let rep = NSBitmapImageRep(bitmapDataPlanes: nil, pixelsWide: Int(size), pixelsHigh: Int(size), bitsPerSample: 8,
                           samplesPerPixel: 4, hasAlpha: true, isPlanar: false, colorSpaceName: .deviceRGB, bytesPerRow: 0, bitsPerPixel: 0)!
NSGraphicsContext.saveGraphicsState()
NSGraphicsContext.current = NSGraphicsContext(bitmapImageRep: rep)
let rect = NSRect(x: inset, y: inset, width: body, height: body)
let shape = NSBezierPath(roundedRect: rect, xRadius: radius, yRadius: radius)

// Shadow under the body.
NSGraphicsContext.saveGraphicsState()
let shadow = NSShadow()
shadow.shadowOffset = NSSize(width: 0, height: -12)
shadow.shadowBlurRadius = 28
shadow.shadowColor = NSColor.black.withAlphaComponent(0.35)
shadow.set()
NSColor.black.setFill()
shape.fill()
NSGraphicsContext.restoreGraphicsState()

// The artwork, clipped to the body.
NSGraphicsContext.saveGraphicsState()
shape.addClip()
art.draw(in: rect, from: .zero, operation: .copy, fraction: 1)
NSGraphicsContext.restoreGraphicsState()

// A hairline highlight on the edge.
let edge = NSBezierPath(roundedRect: rect.insetBy(dx: 1, dy: 1), xRadius: radius - 1, yRadius: radius - 1)
edge.lineWidth = 2
NSColor.white.withAlphaComponent(0.08).setStroke()
edge.stroke()
NSGraphicsContext.restoreGraphicsState()

try! rep.representation(using: .png, properties: [:])!.write(to: URL(fileURLWithPath: args[2]))
