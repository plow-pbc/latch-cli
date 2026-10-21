// A message's text from its `attributedBody`: the NSAttributedString that
// Messages archives in Apple's typedstream format. Ported from imsg's
// TypedStreamParser (openclaw/imsg 2040293, MIT, see LICENSE-imsg): the
// first NSString in the archive is the message text, framed by a length.
//
// It reads bytes and nothing else. It imports no Foundation and never asks
// the Objective-C runtime for a class, so a crafted body cannot make it
// instantiate one (NSUnarchiver, which plow-messages used, can). Message
// bodies come from strangers; every way a body can be wrong ends in a
// `BodyError` for that message, never a crash.

/// Why one message's body could not be read.
public enum BodyError: String, Error, Sendable {
    /// Larger than `Limits.maxBytes`.
    case tooLarge = "body_too_large"
    /// More objects open at once than `Limits.maxDepth` before its text.
    case tooDeep = "body_too_deep"
    /// A length or count that runs past the end of the body.
    case badLength = "body_bad_length"
    /// Decoding took longer than `Limits.budget`.
    case tooSlow = "body_too_slow"
    /// Not a typedstream body, and not text either.
    case notText = "body_not_text"
    /// A typedstream body with no text in it.
    case noText = "body_no_text"
}

/// What one body may cost.
public struct Limits: Sendable {
    public var maxBytes: Int
    public var maxDepth: Int
    public var budget: Duration

    public init(maxBytes: Int = 1 << 20, maxDepth: Int = 64, budget: Duration = .milliseconds(50)) {
        self.maxBytes = maxBytes
        self.maxDepth = maxDepth
        self.budget = budget
    }
}

/// The text of an `attributedBody` blob.
public func decodeAttributedBody(_ bytes: [UInt8], limits: Limits = Limits()) -> Result<String, BodyError> {
    guard bytes.count <= limits.maxBytes else { return .failure(.tooLarge) }
    guard !bytes.isEmpty else { return .success("") }
    if bytes.starts(with: [0xff, 0xfe]) {
        return utf16LittleEndian(bytes.dropFirst(2))
    }
    // Some rows hold plain text where the archive would be.
    guard bytes.starts(with: [4, 11]) else {
        guard let text = valid(bytes, as: UTF8.self) else { return .failure(.notText) }
        return .success(String(text.unicodeScalars.drop(while: isControl)))
    }
    guard bytes.count >= 13 else { return .failure(.badLength) }
    let signature = bytes[2..<13]
    let littleEndian = signature.elementsEqual("streamtyped".utf8)
    guard littleEndian || signature.elementsEqual("typedstream".utf8) else {
        return .failure(.notText)
    }

    let clock = ContinuousClock()
    let deadline = clock.now + limits.budget
    // 0x84 opens an object or class, 0x85 and 0x86 close one: how deep the
    // archive is nested by the time its text starts.
    var depth = 0
    var index = 13
    while index + 1 < bytes.count {
        if index & 0xfff == 0, clock.now > deadline { return .failure(.tooSlow) }
        switch bytes[index] {
        case 0x84:
            depth += 1
            guard depth <= limits.maxDepth else { return .failure(.tooDeep) }
        case 0x85, 0x86:
            depth = max(0, depth - 1)
        case 0x01 where bytes[index + 1] == 0x2b:
            // `+`, an NSString's bytes: the first one is the message text.
            return string(bytes, from: index + 2, littleEndian: littleEndian)
        default:
            break
        }
        index += 1
    }
    return .failure(.noText)
}

/// A length-framed UTF-8 string at `index`: one length byte, or 0x81 and
/// two bytes, or 0x82 and four, in the archive's byte order.
private func string(_ bytes: [UInt8], from index: Int, littleEndian: Bool) -> Result<String, BodyError> {
    guard index < bytes.count else { return .failure(.badLength) }
    let head = bytes[index]
    var start = index + 1
    var length = 0
    if head == 0x81 || head == 0x82 {
        let width = head == 0x81 ? 2 : 4
        guard width <= bytes.count - start else { return .failure(.badLength) }
        for offset in 0..<width {
            let shift = (littleEndian ? offset : width - 1 - offset) * 8
            length |= Int(bytes[start + offset]) << shift
        }
        start += width
    } else {
        // Other tags in this range are not lengths.
        guard !(0x80...0x91).contains(head) else { return .failure(.badLength) }
        length = Int(head)
    }
    guard length <= bytes.count - start else { return .failure(.badLength) }
    guard let text = valid(bytes[start..<(start + length)], as: UTF8.self) else {
        return .failure(.notText)
    }
    return .success(text)
}

private func utf16LittleEndian(_ bytes: ArraySlice<UInt8>) -> Result<String, BodyError> {
    guard bytes.count.isMultiple(of: 2) else { return .failure(.badLength) }
    var units: [UInt16] = []
    units.reserveCapacity(bytes.count / 2)
    var index = bytes.startIndex
    while index < bytes.endIndex {
        units.append(UInt16(bytes[index]) | UInt16(bytes[index + 1]) << 8)
        index += 2
    }
    guard let text = valid(units, as: UTF16.self) else { return .failure(.notText) }
    return .success(text)
}

/// `units` as text, or nil if they are not valid in `encoding`.
private func valid<Units: Sequence, Encoding: Unicode.Encoding>(_ units: Units, as encoding: Encoding.Type)
    -> String? where Units.Element == Encoding.CodeUnit
{
    var parser = Encoding.ForwardParser()
    var iterator = units.makeIterator()
    var scalars = String.UnicodeScalarView()
    while true {
        switch parser.parseScalar(from: &iterator) {
        case .valid(let encoded): scalars.append(Encoding.decode(encoded))
        case .emptyInput: return String(scalars)
        case .error: return nil
        }
    }
}

private func isControl(_ scalar: Unicode.Scalar) -> Bool {
    scalar.properties.generalCategory == .control
}
