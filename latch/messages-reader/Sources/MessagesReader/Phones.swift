// Phone numbers as one canonical handle each, region-aware, through
// PhoneNumberKit (MIT). Messages files a phone under its E.164 form
// (`+14155550100`); the address book keeps whatever was typed
// (`(415) 555-0100`). Both are turned into E.164 with the owner's region
// for numbers written without a country code. What does not parse (a short
// code, an email) is kept as written and only ever matches exactly.

import Foundation
import MachO
import PhoneNumberKit

final class Phones {
    private let utility: PhoneNumberUtility
    let region: String

    init(region: String) {
        self.region = region
        utility = PhoneNumberUtility(metadataCallback: embeddedMetadata)
    }

    /// The canonical handle for `raw`: E.164 for a phone number that parses,
    /// lowercase for an email, otherwise `raw` with its ends trimmed.
    func handle(_ raw: String) -> String {
        let trimmed = raw.trimmingCharacters(in: .whitespacesAndNewlines)
        if trimmed.contains("@") { return trimmed.lowercased() }
        // Only something that looks like a phone number is parsed: digits,
        // spaces and the usual punctuation, with enough digits to be one.
        let digits = trimmed.filter(\.isNumber).count
        let phoneLike = trimmed.allSatisfy { $0.isNumber || " +-().".contains($0) }
        guard phoneLike, digits >= 7,
            let number = try? utility.parse(trimmed, withRegion: region, ignoreType: true)
        else { return trimmed }
        return utility.format(number, toType: .e164)
    }
}

/// PhoneNumberKit's metadata, which the build links into the binary as the
/// section __DATA,__phonemeta (see Package.swift).
private func embeddedMetadata() throws -> Data? {
    var size: UInt = 0
    let image = #dsohandle.assumingMemoryBound(to: mach_header_64.self)
    guard let start = getsectiondata(image, "__DATA", "__phonemeta", &size), size > 0 else {
        throw PhoneNumberError.metadataNotFound
    }
    return Data(bytes: start, count: Int(size))
}
