// tools/_render_template_png.swift — render an SVG to a transparent PNG.
//
// Used by tools/build-icons.sh to rasterise assets/vetter-logo-mono.svg
// into Contents/Resources/StatusItem.png. We use Cocoa (NSImage + a
// CGContext explicitly created with an alpha channel) instead of
// qlmanage because qlmanage's WebKit-based SVG generator flattens
// transparent backgrounds to opaque white — which then renders as a
// solid square in the menu bar once the result is marked
// `setTemplate(true)` and AppKit uses the (all-255) alpha channel as
// the tinting mask.
//
// SVG support on NSImage shipped in macOS 14 (Sonoma); the script
// will fail gracefully on older systems with a clear message.
//
// Usage: swift tools/_render_template_png.swift <input.svg> <output.png> <size>

import Cocoa
import ImageIO
import UniformTypeIdentifiers

let args = CommandLine.arguments
guard args.count == 4, let size = Int(args[3]), size > 0 else {
    FileHandle.standardError.write(
        "usage: \(args[0]) <input.svg> <output.png> <size>\n".data(using: .utf8)!)
    exit(64)
}

let inputPath = args[1]
let outputPath = args[2]

guard let nsImage = NSImage(contentsOfFile: inputPath) else {
    FileHandle.standardError.write(
        "failed to load \(inputPath); macOS 14+ is required for native NSImage SVG support\n"
            .data(using: .utf8)!)
    exit(1)
}

// Explicit RGBA context with premultiplied-last alpha. The default
// init clears every pixel to (0,0,0,0), so anywhere the SVG doesn't
// paint stays fully transparent.
let colorSpace = CGColorSpaceCreateDeviceRGB()
let bitmapInfo = CGImageAlphaInfo.premultipliedLast.rawValue
guard
    let context = CGContext(
        data: nil,
        width: size, height: size,
        bitsPerComponent: 8, bytesPerRow: 0,
        space: colorSpace,
        bitmapInfo: bitmapInfo)
else {
    FileHandle.standardError.write("CGContext init failed\n".data(using: .utf8)!)
    exit(1)
}

let rect = NSRect(x: 0, y: 0, width: size, height: size)
NSGraphicsContext.saveGraphicsState()
NSGraphicsContext.current = NSGraphicsContext(cgContext: context, flipped: false)
nsImage.draw(in: rect, from: .zero, operation: .sourceOver, fraction: 1.0)
NSGraphicsContext.restoreGraphicsState()

guard let cgImage = context.makeImage() else {
    FileHandle.standardError.write("makeImage failed\n".data(using: .utf8)!)
    exit(1)
}

let outURL = URL(fileURLWithPath: outputPath)
guard
    let dest = CGImageDestinationCreateWithURL(
        outURL as CFURL, UTType.png.identifier as CFString, 1, nil)
else {
    FileHandle.standardError.write("CGImageDestinationCreateWithURL failed\n".data(using: .utf8)!)
    exit(1)
}
CGImageDestinationAddImage(dest, cgImage, nil)
guard CGImageDestinationFinalize(dest) else {
    FileHandle.standardError.write("CGImageDestinationFinalize failed\n".data(using: .utf8)!)
    exit(1)
}
