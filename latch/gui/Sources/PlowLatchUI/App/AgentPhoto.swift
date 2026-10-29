import Contacts
import ImageIO

/// A persona's photo, from its line's contact card: the vCard's PHOTO,
/// read without the Contacts store, and drawn small once.
nonisolated enum AgentPhoto {
    /// The pixels a photo is kept at: the detail's 56 pt avatar on a 2x display, and then some.
    static let side = 256

    static func image(fromCard card: Data) -> CGImage? {
        guard let photo = try? CNContactVCardSerialization.contacts(with: card).first?.imageData,
              let source = CGImageSourceCreateWithData(photo as CFData, nil) else { return nil }
        let options = [kCGImageSourceCreateThumbnailFromImageAlways: true, kCGImageSourceThumbnailMaxPixelSize: side,
                       kCGImageSourceCreateThumbnailWithTransform: true] as CFDictionary
        return CGImageSourceCreateThumbnailAtIndex(source, 0, options)
    }
}
