import Foundation
import MachO
import PhoneNumberKit

/// A phone number in E.164 (`+16505550123`), as Plow gives a line's,
/// written as people in its region write it, through PhoneNumberKit (MIT).
struct PhoneNumber: Equatable {
    var e164: String

    init?(_ text: String) {
        let digits = text.dropFirst()
        guard text.first == "+", (8...15).contains(digits.count),
              digits.allSatisfy({ $0.isASCII && $0.isNumber }) else { return nil }
        e164 = text
    }

    /// Its `sms:` link: Messages sends iMessage to it when it can.
    var sms: URL? { URL(string: "sms:\(e164)") }

    /// As people write it: as its own country writes it on a Mac set to that
    /// region, else in international form; as given if it doesn't parse.
    func formatted(region: String? = Locale.current.region?.identifier) -> String {
        // A region is always given: PhoneNumberKit's own default asks the
        // Contacts framework. A number with its country code ignores it.
        let region = region ?? "US"
        guard let number = try? Self.utility.parse(e164, withRegion: region, ignoreType: true)
        else { return e164 }
        return Self.utility.format(number, toType: Self.utility.getRegionCode(of: number) == region ? .national : .international)
    }

    private static let utility = PhoneNumberUtility(metadataCallback: embeddedMetadata)
}

/// PhoneNumberKit's metadata, which the build links into the image as the
/// section __DATA,__phonemeta (see Package.swift).
private nonisolated func embeddedMetadata() throws -> Data? {
    var size: UInt = 0
    let image = #dsohandle.assumingMemoryBound(to: mach_header_64.self)
    guard let start = getsectiondata(image, "__DATA", "__phonemeta", &size), size > 0 else {
        throw PhoneNumberError.metadataNotFound
    }
    return Data(bytes: start, count: Int(size))
}
