import CoreImage
import CoreImage.CIFilterBuiltins
import SwiftUI

enum QRCode {
    /// `payload` as a QR code, twelve pixels per module, black on white,
    /// with medium error correction: sharp however it's scaled.
    static func image(_ payload: String) -> CGImage? {
        let filter = CIFilter.qrCodeGenerator()
        filter.message = Data(payload.utf8)
        filter.correctionLevel = "M"
        guard let output = filter.outputImage?.transformed(by: CGAffineTransform(scaleX: 12, y: 12)) else { return nil }
        return context.createCGImage(output, from: output.extent)
    }

    /// Made once: making a context costs more than drawing with it, and a
    /// view asks for its code each time it's drawn.
    private static let context = CIContext()
}

/// The QR code for `payload`, drawn crisp at any size.
struct QRCodeView: View {
    var payload: String

    var body: some View {
        if let image = QRCode.image(payload) {
            Image(decorative: image, scale: 1)
                .interpolation(.none)
                .resizable()
                .aspectRatio(1, contentMode: .fit)
        }
    }
}
