// A message's text from its `attributedBody`: the NSAttributedString that
// Messages archives in Apple's typedstream format (NSArchiver's). The text
// is the archive's first string (`+`). The format and its tags follow
// imsg's TypedStreamParser (openclaw/imsg 2040293, MIT, see LICENSE-imsg)
// and imessage-exporter's description of typedstream.
//
// It reads bytes and nothing else. It imports no Foundation and never asks
// the Objective-C runtime for a class, so a crafted body cannot make it
// instantiate one (NSUnarchiver, which plow-messages used, can).
//
// The whole archive is parsed, not only up to the text: every object it
// opens counts toward `Limits.maxDepth`, every length and reference must
// stay inside what has been read, and every path (plain text, UTF-16, the
// archive and the text in it) spends from one time budget. Message bodies
// come from strangers; every way a body can be wrong ends in a `BodyError`
// for that message, never a crash.

/// Why one message's body could not be read.
public enum BodyError: String, Error, Sendable {
    /// Larger than `Limits.maxBytes`.
    case tooLarge = "body_too_large"
    /// More objects open at once than `Limits.maxDepth`.
    case tooDeep = "body_too_deep"
    /// A length that runs past the end of the body, or an archive that ends
    /// inside an object.
    case badLength = "body_bad_length"
    /// Decoding took longer than `Limits.budget`.
    case tooSlow = "body_too_slow"
    /// Not a typedstream body, and not text either.
    case notText = "body_not_text"
    /// A typedstream body with no text in it.
    case noText = "body_no_text"
    /// An archive that does not follow typedstream's grammar: an unknown
    /// type, or a reference to something not yet read.
    case badStructure = "body_bad_structure"
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
    var budget = Budget(limits.budget)
    do {
        try budget.check(now: true)
        if bytes.isEmpty { return .success("") }
        if bytes.starts(with: [0xff, 0xfe]) {
            let body = bytes.dropFirst(2)
            guard body.count.isMultiple(of: 2) else { throw BodyError.badLength }
            let units = stride(from: body.startIndex, to: body.endIndex, by: 2).lazy.map {
                UInt16(body[$0]) | UInt16(body[$0 + 1]) << 8
            }
            return .success(try valid(units, as: UTF16.self, &budget))
        }
        // Some rows hold plain text where the archive would be.
        guard bytes.starts(with: [4, 11]) else {
            let text = try valid(bytes, as: UTF8.self, &budget)
            return .success(String(text.unicodeScalars.drop(while: isControl)))
        }
        var archive = Archive(bytes: bytes, maxDepth: limits.maxDepth, budget: budget)
        return .success(try archive.text())
    } catch let error as BodyError {
        return .failure(error)
    } catch {
        return .failure(.badStructure)
    }
}

/// Typedstream's tags: a 2- or 4-byte integer follows, a float follows, a
/// new item starts, nil, an object ends; from `reference` up, a byte is a
/// reference to an item already read.
private let int16: UInt8 = 0x81
private let int32: UInt8 = 0x82
private let decimal: UInt8 = 0x83
private let start: UInt8 = 0x84
private let empty: UInt8 = 0x85
private let end: UInt8 = 0x86
private let reference: Int64 = 0x92

/// The time a body may take, checked as decoding goes.
private struct Budget {
    let clock = ContinuousClock()
    let deadline: ContinuousClock.Instant
    var steps = 0

    init(_ budget: Duration) { deadline = clock.now + budget }

    /// Every 256 steps (or `now`), fails if the time is up.
    mutating func check(now: Bool = false) throws {
        steps += 1
        if (now || steps & 0xff == 0) && clock.now >= deadline { throw BodyError.tooSlow }
    }
}

private struct Archive {
    let bytes: [UInt8]
    let maxDepth: Int
    var budget: Budget
    var index = 0
    var littleEndian = true
    /// Type strings and class names, in the order read: what a type
    /// group's reference points into.
    var types: [[UInt8]] = []
    /// Objects and classes read so far, which a reference may name.
    var objects: Int64 = 0
    /// Objects open now.
    var depth = 0
    var found: String?

    init(bytes: [UInt8], maxDepth: Int, budget: Budget) {
        self.bytes = bytes
        self.maxDepth = maxDepth
        self.budget = budget
    }

    /// Parses the whole archive and returns its first string.
    mutating func text() throws -> String {
        guard bytes.count >= 13 else { throw BodyError.badLength }
        let signature = bytes[2..<13]
        littleEndian = signature.elementsEqual("streamtyped".utf8)
        guard littleEndian || signature.elementsEqual("typedstream".utf8) else { throw BodyError.notText }
        index = 13
        _ = try integer()  // the system version
        while index < bytes.count {
            try budget.check()
            if bytes[index] == end {
                index += 1
                depth = max(0, depth - 1)
                continue
            }
            let group = try typeGroup()
            var position = 0
            while position < group.count {
                try value(group, &position)
            }
        }
        // Ending inside an object means the archive was cut short.
        guard depth == 0 else { throw BodyError.badLength }
        guard let found else { throw BodyError.noText }
        return found
    }

    func byte() throws -> UInt8 {
        guard index < bytes.count else { throw BodyError.badLength }
        return bytes[index]
    }

    mutating func take(_ count: Int64) throws -> ArraySlice<UInt8> {
        guard count >= 0, count <= Int64(bytes.count - index) else { throw BodyError.badLength }
        let slice = bytes[index..<(index + Int(count))]
        index += Int(count)
        return slice
    }

    /// An integer: one signed byte, or a tag and two or four bytes in the
    /// archive's byte order.
    mutating func integer(unsigned: Bool = false) throws -> Int64 {
        let tag = try byte()
        index += 1
        let width: Int
        switch tag {
        case int16: width = 2
        case int32: width = 4
        default:
            guard unsigned else { return Int64(Int8(bitPattern: tag)) }
            // A byte at or above 0x80 that is not a tag is no length.
            guard tag < 0x80 else { throw BodyError.badLength }
            return Int64(tag)
        }
        let raw = try take(Int64(width))
        var value: UInt64 = 0
        for (offset, part) in raw.enumerated() {
            let shift = (littleEndian ? offset : width - 1 - offset) * 8
            value |= UInt64(part) << UInt64(shift)
        }
        if unsigned { return Int64(value) }
        return width == 2 ? Int64(Int16(truncatingIfNeeded: value)) : Int64(Int32(truncatingIfNeeded: value))
    }

    /// A reference to an item already read: an index below `limit`.
    mutating func referenceTo(_ limit: Int64) throws -> Int64 {
        let tag = try byte()
        let value = tag == int16 || tag == int32 ? try integer() : Int64(tag)
        if tag != int16 && tag != int32 { index += 1 }
        let target = value - reference
        guard target >= 0, target < limit else { throw BodyError.badStructure }
        return target
    }

    /// A new string (a type or a class name), or a reference to one.
    mutating func sharedString() throws -> [UInt8] {
        if try byte() == start {
            index += 1
            let string = Array(try take(try integer(unsigned: true)))
            types.append(string)
            return string
        }
        return types[Int(try referenceTo(Int64(types.count)))]
    }

    /// The types of the values that follow.
    mutating func typeGroup() throws -> [UInt8] {
        let group = try sharedString()
        guard !group.isEmpty else { throw BodyError.badStructure }
        return group
    }

    /// One value of the type at `position` in `group`.
    mutating func value(_ group: [UInt8], _ position: inout Int) throws {
        try budget.check()
        let type = group[position]
        position += 1
        switch type {
        case UInt8(ascii: "@"), UInt8(ascii: "#"):
            try object()
        case UInt8(ascii: "+"):
            // An NSString's bytes: the first is the message's text.
            let string = try take(try integer(unsigned: true))
            if found == nil {
                found = try valid(string, as: UTF8.self, &budget)
            }
        case UInt8(ascii: "*"):
            // A C string (an NSValue's type, say): nil, a new one (an object
            // as well as a shared string), or one read before.
            let tag = try byte()
            if tag == empty {
                index += 1
            } else if tag == start {
                index += 1
                if try byte() == start { objects += 1 }
                _ = try sharedString()
            } else {
                _ = try referenceTo(objects)
            }
        case UInt8(ascii: ":"):
            _ = try sharedString()
        case UInt8(ascii: "c"), UInt8(ascii: "C"), UInt8(ascii: "s"), UInt8(ascii: "S"), UInt8(ascii: "i"),
            UInt8(ascii: "I"), UInt8(ascii: "l"), UInt8(ascii: "L"), UInt8(ascii: "q"), UInt8(ascii: "Q"),
            UInt8(ascii: "B"):
            _ = try integer()
        case UInt8(ascii: "f"), UInt8(ascii: "d"):
            if try byte() == decimal {
                index += 1
                _ = try take(type == UInt8(ascii: "f") ? 4 : 8)
            } else {
                _ = try integer()
            }
        case UInt8(ascii: "["):
            // [N<type>]: N values of one type; N bytes as they are.
            var count: Int64 = 0
            while position < group.count, let digit = Int64(exactly: group[position]),
                (48...57).contains(digit)
            {
                count = count * 10 + digit - 48
                guard count <= Int64(bytes.count) else { throw BodyError.badLength }
                position += 1
            }
            let element = try span(group, position)
            guard element.upperBound < group.count, group[element.upperBound] == UInt8(ascii: "]") else {
                throw BodyError.badStructure
            }
            if element.count == 1 && [UInt8(ascii: "c"), UInt8(ascii: "C")].contains(group[element.lowerBound]) {
                _ = try take(count)
            } else {
                // Each value takes at least a byte: more than there are left is a lie.
                guard count <= Int64(bytes.count - index) else { throw BodyError.badLength }
                for _ in 0..<count {
                    var inner = element.lowerBound
                    try value(group, &inner)
                }
            }
            position = element.upperBound + 1
        case UInt8(ascii: "{"):
            // {name=<types>}: each field in turn.
            while position < group.count && group[position] != UInt8(ascii: "=") && group[position] != UInt8(ascii: "}") {
                position += 1
            }
            if position < group.count && group[position] == UInt8(ascii: "=") { position += 1 }
            while position < group.count && group[position] != UInt8(ascii: "}") {
                try value(group, &position)
            }
            guard position < group.count else { throw BodyError.badStructure }
            position += 1
        default:
            throw BodyError.badStructure
        }
    }

    /// The span of the one type that starts at `position`, brackets and
    /// braces included.
    func span(_ group: [UInt8], _ position: Int) throws -> Range<Int> {
        guard position < group.count else { throw BodyError.badStructure }
        var level = 0
        var at = position
        repeat {
            guard at < group.count else { throw BodyError.badStructure }
            switch group[at] {
            case UInt8(ascii: "["), UInt8(ascii: "{"): level += 1
            case UInt8(ascii: "]"), UInt8(ascii: "}"): level -= 1
            default: break
            }
            at += 1
            guard level <= maxDepth else { throw BodyError.tooDeep }
        } while level > 0
        return position..<at
    }

    /// An object: a new one (its class chain, then its contents as type
    /// groups up to its `end`), nil, or a reference to one read before.
    mutating func object() throws {
        let tag = try byte()
        if tag == empty {
            index += 1
            return
        }
        guard tag == start else {
            _ = try referenceTo(objects)
            return
        }
        index += 1
        objects += 1
        depth += 1
        guard depth <= maxDepth else { throw BodyError.tooDeep }
        // The class, then each superclass, to nil or a class read before.
        for _ in 0...maxDepth {
            try budget.check()
            let next = try byte()
            if next == empty {
                index += 1
                return
            }
            guard next == start else {
                _ = try referenceTo(objects)
                return
            }
            index += 1
            _ = try sharedString()  // the class name
            _ = try integer()  // its version
            objects += 1
        }
        throw BodyError.tooDeep
    }
}

/// `units` as text, or `notText` if they are not valid in `encoding`.
private func valid<Units: Sequence, Encoding: Unicode.Encoding>(
    _ units: Units, as encoding: Encoding.Type, _ budget: inout Budget
) throws -> String where Units.Element == Encoding.CodeUnit {
    var parser = Encoding.ForwardParser()
    var iterator = units.makeIterator()
    var scalars = String.UnicodeScalarView()
    while true {
        try budget.check()
        switch parser.parseScalar(from: &iterator) {
        case .valid(let encoded): scalars.append(Encoding.decode(encoded))
        case .emptyInput: return String(scalars)
        case .error: throw BodyError.notText
        }
    }
}

private func isControl(_ scalar: Unicode.Scalar) -> Bool {
    scalar.properties.generalCategory == .control
}
