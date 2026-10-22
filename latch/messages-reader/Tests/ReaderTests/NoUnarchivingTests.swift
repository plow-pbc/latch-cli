// The reader decodes bodies from their bytes alone: nothing in it can turn
// a message into an object of a class the message names.

import Foundation
import Testing

@Test
func theReaderNeverUnarchives() throws {
    let sources = URL(fileURLWithPath: #filePath).deletingLastPathComponent()
        .appendingPathComponent("../../Sources").standardized
    let files = FileManager.default.enumerator(at: sources, includingPropertiesForKeys: nil)!
        .compactMap { $0 as? URL }.filter { $0.pathExtension == "swift" }
    #expect(files.count >= 7)
    let banned = ["NSUnarchiver", "NSKeyedUnarchiver", "NSCoder", "NSClassFromString", "objc_getClass",
                  "unarchive", "decodeObject", "NSSecureCoding",
                  // Names come from the address book's files, never the Contacts framework, which prompts.
                  "CNContactStore", "import Contacts",
                  // PhoneNumberKit's own default region asks the Contacts framework; a region is always given.
                  "defaultRegionCode"]
    for file in files {
        let source = try String(contentsOf: file, encoding: .utf8)
        // Comments may name what the reader avoids; code may not.
        let code = source.split(separator: "\n").filter { !$0.trimmingCharacters(in: .whitespaces).hasPrefix("//") }
            .joined(separator: "\n")
        for word in banned {
            #expect(!code.contains(word), "\(file.lastPathComponent) uses \(word)")
        }
        if file.path.contains("/TypedStream/") {
            #expect(!code.contains("import"), "the body parser imports nothing")
        }
    }
}
