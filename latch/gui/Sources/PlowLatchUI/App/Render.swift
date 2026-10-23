#if DEBUG || LATCH_TEST_HOOKS
import AppKit

/// A view, drawn into a PNG from its own layers: no screen recording, so
/// nothing asks for permission.
enum Render {
    static func png(_ view: NSView, to file: URL) throws {
        view.layoutSubtreeIfNeeded()
        guard let rep = view.bitmapImageRepForCachingDisplay(in: view.bounds),
              let context = NSGraphicsContext(bitmapImageRep: rep), let layer = view.layer
        else { throw CocoaError(.fileWriteUnknown) }
        NSGraphicsContext.current = context
        (view.window?.effectiveAppearance ?? NSApp.effectiveAppearance).performAsCurrentDrawingAppearance {
            NSColor.windowBackgroundColor.setFill()
            view.bounds.fill()
        }
        let cg = context.cgContext
        cg.translateBy(x: 0, y: view.bounds.height)
        cg.scaleBy(x: 1, y: -1)
        layer.render(in: cg)
        NSGraphicsContext.current = nil
        guard let png = rep.representation(using: .png, properties: [:]) else { throw CocoaError(.fileWriteUnknown) }
        try png.write(to: file)
    }
}
#endif
