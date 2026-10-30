#if DEBUG || LATCH_TEST_HOOKS
import AppKit

/// A view, drawn into a PNG from its own layers: no screen recording, so
/// nothing asks for permission.
enum Render {
    /// `background: false` keeps what the view leaves clear transparent,
    /// as a floating panel's rounded corners.
    static func png(_ view: NSView, to file: URL, background: Bool = true) throws {
        view.layoutSubtreeIfNeeded()
        guard let rep = view.bitmapImageRepForCachingDisplay(in: view.bounds),
              let context = NSGraphicsContext(bitmapImageRep: rep), let layer = view.layer
        else { throw CocoaError(.fileWriteUnknown) }
        NSGraphicsContext.current = context
        if background {
            (view.window?.effectiveAppearance ?? NSApp.effectiveAppearance).performAsCurrentDrawingAppearance {
                NSColor.windowBackgroundColor.setFill()
                view.bounds.fill()
            }
        }
        let cg = context.cgContext
        cg.translateBy(x: 0, y: view.bounds.height)
        cg.scaleBy(x: 1, y: -1)
        layer.render(in: cg)
        NSGraphicsContext.current = nil
        guard let png = rep.representation(using: .png, properties: [:]) else { throw CocoaError(.fileWriteUnknown) }
        try png.write(to: file)
    }

    /// A window of this app as the window server shows it, system materials,
    /// sidebar and toolbar included, even off screen. A process may read
    /// its own windows without screen-recording permission. Swift no longer
    /// offers the call (ScreenCaptureKit replaced it, and asks permission
    /// even for one's own windows), so it's looked up at run time.
    static func window(_ window: NSWindow, to file: URL) throws {
        guard let image = image(window), let png = NSBitmapImageRep(cgImage: image).representation(using: .png, properties: [:])
        else { throw CocoaError(.fileWriteUnknown) }
        try png.write(to: file)
    }

    /// The window as the window server shows it now.
    static func image(_ window: NSWindow) -> CGImage? {
        typealias CreateImage = @convention(c) (CGRect, UInt32, UInt32, UInt32) -> Unmanaged<CGImage>?
        let includingWindow: UInt32 = 1 << 3, ignoreFraming: UInt32 = 1 << 0
        guard let symbol = dlsym(UnsafeMutableRawPointer(bitPattern: -2), "CGWindowListCreateImage") else { return nil }
        return unsafeBitCast(symbol, to: CreateImage.self)(.null, includingWindow, UInt32(window.windowNumber), ignoreFraming)?
            .takeRetainedValue()
    }
}
#endif
