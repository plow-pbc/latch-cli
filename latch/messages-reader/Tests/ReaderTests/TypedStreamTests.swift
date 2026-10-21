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
    let bigEndian: [UInt8] = [4, 11] + Array("typedstream".utf8) + [0x81, 0x03, 0xe8, 0x01, 0x2b, 0x81, 0x01, 0x2c]
        + Array(String(repeating: "b", count: 300).utf8)
    #expect(try decodeAttributedBody(bigEndian).get() == String(repeating: "b", count: 300))
}

@Test
func eachLimitRejectsItsBody() throws {
    #expect(decodeAttributedBody(Malformed.oversized) == .failure(.tooLarge))
    #expect(decodeAttributedBody(Malformed.deep) == .failure(.tooDeep))
    #expect(decodeAttributedBody(Malformed.lying) == .failure(.badLength))
    #expect(decodeAttributedBody(Malformed.slow, limits: Limits(budget: .zero)) == .failure(.tooSlow))
    // With the usual budget the same body is merely textless.
    #expect(decodeAttributedBody(Malformed.slow) == .failure(.noText))
    // 64 open objects are allowed.
    let deepest = header + [UInt8](repeating: 0x84, count: 64) + [0x01, 0x2b, 2] + Array("ok".utf8)
    #expect(try decodeAttributedBody(deepest).get() == "ok")
}

@Test
func truncatedAndInvalidFramesAreErrors() throws {
    let start = header + [0x01, 0x2b]
    for payload: [UInt8] in [[], [0x81], [0x81, 0x01], [0x82, 1, 0, 0], [0x85], [5, 0x61, 0x62]] {
        #expect(decodeAttributedBody(start + payload) == .failure(.badLength), "\(payload)")
    }
    #expect(decodeAttributedBody(start + [1, 0xff]) == .failure(.notText))
    #expect(decodeAttributedBody([4, 11] + Array("streamtype".utf8)) == .failure(.badLength))
    #expect(decodeAttributedBody([4, 11] + Array("notastream!".utf8)) == .failure(.notText))
    #expect(decodeAttributedBody([0xff, 0xfe, 0x61]) == .failure(.badLength))
    #expect(decodeAttributedBody([0xc3, 0x28]) == .failure(.notText))
}

@Test
func theLargestBodyDecodesWellInsideItsBudget() throws {
    // The worst case the limits allow: a full megabyte of object markers.
    let worst = header + [UInt8](repeating: 0x85, count: (1 << 20) - header.count)
    let started = ContinuousClock.now
    #expect(decodeAttributedBody(worst) == .failure(.noText))
    #expect(ContinuousClock.now - started < .milliseconds(50))
}
