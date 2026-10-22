// A bounded fuzz run of the body parser, seeded from the fixtures: every
// mutation must decode or fail cleanly, never crash or run long. A crash
// fails the whole test run. LATCH_FUZZ_ITERATIONS sets the count.

import Foundation
import Testing
import TypedStream

/// A small seeded generator, so a failing run can be repeated.
struct SplitMix: RandomNumberGenerator {
    var state: UInt64
    mutating func next() -> UInt64 {
        state &+= 0x9e37_79b9_7f4a_7c15
        var z = state
        z = (z ^ (z >> 30)) &* 0xbf58_476d_1ce4_e5b9
        z = (z ^ (z >> 27)) &* 0x94d0_49bb_1331_11eb
        return z ^ (z >> 31)
    }
}

/// The bytes typedstream gives meaning to, which mutations favour.
let tags: [UInt8] = [0x00, 0x01, 0x2b, 0x40, 0x7f, 0x80, 0x81, 0x82, 0x84, 0x85, 0x86, 0x91, 0x92, 0xfe, 0xff]

func mutate(_ input: [UInt8], _ random: inout SplitMix) -> [UInt8] {
    var bytes = input
    for _ in 0..<Int.random(in: 1...8, using: &random) {
        let at = bytes.isEmpty ? 0 : Int.random(in: 0..<bytes.count, using: &random)
        switch Int.random(in: 0..<6, using: &random) {
        case 0 where !bytes.isEmpty: bytes[at] ^= 1 << UInt8.random(in: 0..<8, using: &random)
        case 1 where !bytes.isEmpty: bytes[at] = tags.randomElement(using: &random)!
        case 2: bytes = Array(bytes.prefix(at))
        case 3: bytes.insert(contentsOf: (0..<Int.random(in: 1...16, using: &random)).map { _ in
            UInt8.random(in: 0...255, using: &random)
        }, at: at)
        case 4 where !bytes.isEmpty:
            let end = min(bytes.count, at + Int.random(in: 1...64, using: &random))
            bytes.insert(contentsOf: bytes[at..<end], at: at)
        default:
            // A length field that lies: 0x81 or 0x82 and random width bytes.
            let width = Bool.random(using: &random) ? 2 : 4
            let field = [UInt8(width == 2 ? 0x81 : 0x82)] + (0..<width).map { _ in UInt8.random(in: 0...255, using: &random) }
            bytes.insert(contentsOf: field, at: at)
        }
    }
    return bytes
}

@Test
func fuzzedBodiesNeverCrashOrRunLong() {
    let iterations = Int(ProcessInfo.processInfo.environment["LATCH_FUZZ_ITERATIONS"] ?? "") ?? 20_000
    let seeds: [[UInt8]] = [
        archived("Your code is 482913"), archived(String(repeating: "x", count: 300)),
        archived("see this", attributes: [NSAttributedString.Key("NSLink"): URL(string: "https://example.com")!]),
        archivedMutable("mutable 🎉"), captured("typedstream-order-delivered"), captured("typedstream-costco-delivery"),
        Malformed.deep, Malformed.lying, Array(Malformed.slow.prefix(4096)),
        [0xff, 0xfe, 0x61, 0x00], Array("plain".utf8),
    ]
    var random = SplitMix(state: 0x1a7c_4e55)
    var outcomes: [String: Int] = [:]
    var slowest = Duration.zero
    for _ in 0..<iterations {
        let body = mutate(seeds.randomElement(using: &random)!, &random)
        let started = ContinuousClock.now
        let result = decodeAttributedBody(body)
        slowest = max(slowest, ContinuousClock.now - started)
        switch result {
        case .success: outcomes["text", default: 0] += 1
        case .failure(let error): outcomes[error.rawValue, default: 0] += 1
        }
    }
    print("fuzz: \(iterations) bodies, slowest \(slowest), outcomes \(outcomes.sorted { $0.key < $1.key })")
    #expect(outcomes.values.reduce(0, +) == iterations)
    // Each body is held to 50 ms; the rest is room for a busy machine.
    #expect(slowest < .seconds(1))
}
