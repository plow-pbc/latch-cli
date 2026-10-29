import Foundation

/// A phone number in E.164 (`+16505550123`), as Plow gives a line's. macOS
/// has no public formatter for one, so its country code is read from the
/// ITU's assignments and its digits are grouped here.
nonisolated struct PhoneNumber: Equatable {
    /// The country calling code, without its `+`.
    var country: String
    /// The digits after it.
    var national: String

    init?(_ text: String) {
        let digits = text.dropFirst()
        guard text.first == "+", (8...15).contains(digits.count),
              digits.allSatisfy({ $0.isASCII && $0.isNumber }) else { return nil }
        let length = Self.oneDigit.contains(digits.prefix(1)) ? 1 : Self.twoDigit.contains(String(digits.prefix(2))) ? 2 : 3
        country = String(digits.prefix(length))
        national = String(digits.dropFirst(length))
    }

    /// Its `sms:` link: Messages sends iMessage to it when it can.
    var sms: URL? { URL(string: "sms:+\(country)\(national)") }

    /// As people write it: a North American number as (650) 555-0123 on a
    /// Mac set to the US or Canada, else +1 650-555-0123; any other as its
    /// country code, then its digits in threes, ending in four, with no
    /// group of one.
    func formatted(region: String? = Locale.current.region?.identifier) -> String {
        if country == "1", national.count == 10 {
            let area = national.prefix(3), exchange = national.dropFirst(3).prefix(3), line = national.suffix(4)
            return ["US", "CA"].contains(region) ? "(\(area)) \(exchange)-\(line)" : "+1 \(area)-\(exchange)-\(line)"
        }
        var groups = [String(national.suffix(4))]
        var rest = national.dropLast(4)
        while rest.count > 3 {
            groups.insert(String(rest.suffix(3)), at: 0)
            rest = rest.dropLast(3)
        }
        // A lone digit leads no group: it joins the next.
        if rest.count == 1 { groups[0] = rest + groups[0] } else if !rest.isEmpty { groups.insert(String(rest), at: 0) }
        return "+\(country) " + groups.joined(separator: " ")
    }

    /// The codes of one and two digits (ITU-T E.164); every other is three.
    private static let oneDigit: Set<Substring> = ["1", "7"]
    private static let twoDigit: Set<String> = [
        "20", "27", "30", "31", "32", "33", "34", "36", "39", "40", "41", "43", "44", "45", "46", "47", "48", "49",
        "51", "52", "53", "54", "55", "56", "57", "58", "60", "61", "62", "63", "64", "65", "66",
        "81", "82", "84", "86", "90", "91", "92", "93", "94", "95", "98",
    ]
}
