// The body parser, alone: real-world shapes decode, and every limit turns a
// bad body into an error for that body.

import Foundation
import Testing
import TypedStream

@Test(arguments: [0, 5, 126, 127, 128, 255, 256, 300, 8192, 32768, 70000])
func bodiesOfEveryLengthFrameDecode(length: Int) throws {
    let text = String(repeating: "a", count: length)
    #expect(try decodeAttributedBody(archived(text)).get() == text)
}

@Test(arguments: ["left ↄ right", "🎉🎉🎉🎉🎉🎉🎉🎉🎉🎉", "\n\nhello", "left\u{1}+right", "code 123456"])
func textIsKeptAsWritten(text: String) throws {
    #expect(try decodeAttributedBody(archived(text)).get() == text)
}

@Test
func capturedBodiesDecode() throws {
    #expect(
        try decodeAttributedBody(captured("typedstream-order-delivered")).get()
            == "Your order was delivered! Thank you for ordering from Super Duper Burgers.")
    let costco = try decodeAttributedBody(captured("typedstream-costco-delivery")).get()
    #expect(costco.hasPrefix("Your Costco order will arrive shortly!"), "\(costco)")
}

@Test
func attributeValuesAreNotTheText() throws {
    let link = archived("see this", attributes: [NSAttributedString.Key("NSLink"): URL(string: "https://example.com/a")!])
    #expect(try decodeAttributedBody(link).get() == "see this")
    let long = archived("hello", attributes: [NSAttributedString.Key("k"): String(repeating: "x", count: 300)])
    #expect(try decodeAttributedBody(long).get() == "hello")
    #expect(try decodeAttributedBody(archivedMutable("mutable")).get() == "mutable")
}

@Test
func otherBodyFormsDecode() throws {
    let utf16: [UInt8] = [0xff, 0xfe] + Array("hi ☀️".utf16).flatMap { [UInt8($0 & 0xff), UInt8($0 >> 8)] }
    #expect(try decodeAttributedBody(utf16).get() == "hi ☀️")
    #expect(try decodeAttributedBody([0x00, 0x0a] + Array("plain".utf8)).get() == "plain")
    #expect(try decodeAttributedBody([]).get() == "")
    let long = String(repeating: "b", count: 300)
    #expect(try decodeAttributedBody(archive(long, littleEndian: false)).get() == long)
}

@Test
func moreArchivedShapesDecode() throws {
    let attributes: [NSAttributedString.Key: Any] = [
        NSAttributedString.Key("__kIMMessagePartAttributeName"): NSNumber(value: 0),
        NSAttributedString.Key("__kIMDataDetectedAttributeName"): Data(repeating: 7, count: 300),
        NSAttributedString.Key("NSLink"): URL(string: "https://example.com/a")!,
        NSAttributedString.Key("double"): NSNumber(value: 2.5),
        NSAttributedString.Key("bool"): NSNumber(value: true),
        NSAttributedString.Key("date"): Date(timeIntervalSince1970: 1_700_000_000),
        NSAttributedString.Key("list"): ["a", NSNumber(value: 3), ["nested": "x"]] as NSArray,
        NSAttributedString.Key("range"): NSValue(range: NSRange(location: 2, length: 5)),
    ]
    let text = "Your code is 482913 🎉"
    let body = NSMutableAttributedString(string: text)
    for (offset, (key, value)) in attributes.enumerated() {
        body.addAttribute(key, value: value, range: NSRange(location: offset, length: 2))
    }
    let bytes = [UInt8](NSArchiver.archivedData(withRootObject: body))
    #expect(try decodeAttributedBody(bytes).get() == text)
}

@Test
func eachLimitRejectsItsBodyEvenAfterTheText() throws {
    #expect(decodeAttributedBody(Malformed.oversized) == .failure(.tooLarge))
    #expect(decodeAttributedBody(Malformed.deep) == .failure(.tooDeep))
    #expect(decodeAttributedBody(Malformed.lying) == .failure(.badLength))
    // 64 objects open at once are allowed.
    #expect(try decodeAttributedBody(archive("ok", inside: nested(63))).get() == "ok")
    // A reference to a type or object not yet read.
    #expect(decodeAttributedBody(archive("ok", inside: [0x99, 0x00])) == .failure(.badStructure))
    #expect(decodeAttributedBody(archive("ok", inside: [0x92, 0x9f])) == .failure(.badStructure))
    // An unknown type.
    #expect(decodeAttributedBody(archive("ok", inside: [0x84, 0x01, 0x7e, 0x00])) == .failure(.badStructure))
    // Structs inside structs nest like objects: 65 of them (66 with the
    // string's object) is too deep, 63 is not.
    let structs = { (count: Int) -> [UInt8] in
        let type = Array(String(repeating: "{=", count: count).utf8) + [UInt8(ascii: "i")]
            + [UInt8](repeating: UInt8(ascii: "}"), count: count)
        return [0x84] + length(type.count) + type + [0x00]
    }
    #expect(decodeAttributedBody(archive("ok", inside: structs(65))) == .failure(.tooDeep))
    #expect(try decodeAttributedBody(archive("ok", inside: structs(63))).get() == "ok")
    // Arrays of structs, too.
    let arrays = Array("[1".utf8) + Array(String(repeating: "{=", count: 70).utf8) + [UInt8(ascii: "i")]
        + [UInt8](repeating: UInt8(ascii: "}"), count: 70) + [UInt8(ascii: "]")]
    #expect(decodeAttributedBody(archive("ok", inside: [0x84] + length(arrays.count) + arrays + [0x00]))
        == .failure(.tooDeep))
    // Cut short: the string's object never ends.
    #expect(decodeAttributedBody(archive("ok", end: false)) == .failure(.badLength))
}

@Test
func everyPathSpendsFromTheBudget() throws {
    let none = Limits(budget: .zero)
    for body in [archive("ok"), [0xff, 0xfe, 0x6f, 0x00], Array("plain".utf8)] {
        #expect(decodeAttributedBody(body, limits: none) == .failure(.tooSlow), "\(body.prefix(4))")
    }
    // A body that takes long to walk runs out of a small budget part way;
    // with the usual one it reads.
    #expect(decodeAttributedBody(Malformed.slow, limits: Limits(budget: .microseconds(100))) == .failure(.tooSlow))
    #expect(try decodeAttributedBody(Malformed.slow).get() == "slow")
}

@Test
func truncatedAndInvalidFramesAreErrors() {
    let start = Array(archive("", end: false).dropLast())
    for payload: [UInt8] in [[], [0x81], [0x81, 0x01], [0x82, 1, 0, 0], [0x85], [5, 0x61, 0x62]] {
        #expect(decodeAttributedBody(start + payload) == .failure(.badLength), "\(payload)")
    }
    #expect(decodeAttributedBody(start + [1, 0xff, 0x86]) == .failure(.notText))
    #expect(decodeAttributedBody([4, 11] + Array("streamtype".utf8)) == .failure(.badLength))
    #expect(decodeAttributedBody([4, 11] + Array("notastream!".utf8)) == .failure(.notText))
    #expect(decodeAttributedBody([0xff, 0xfe, 0x61]) == .failure(.badLength))
    #expect(decodeAttributedBody([0xc3, 0x28]) == .failure(.notText))
}

@Test
func theCostliestBodyStopsWithinItsBudget() {
    // The worst the size limit allows: half a million of the smallest values.
    let worst = archive("ok", inside: [0x84, 0x01, 0x69, 0x00] + Array([[UInt8]](repeating: [0x95, 0x00], count: 500_000).joined()))
    let started = ContinuousClock.now
    let result = decodeAttributedBody(worst)
    // Read, or refused as too slow; either way, about when the budget ends
    // (with room for a machine busy with other work).
    #expect(result == .success("ok") || result == .failure(.tooSlow))
    #expect(ContinuousClock.now - started < .seconds(1))
}
