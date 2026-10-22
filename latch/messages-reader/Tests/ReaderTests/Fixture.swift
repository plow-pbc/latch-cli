// A synthetic chat.db: only the tables and columns the reader queries,
// named as Messages names them. No real message is ever read; the two
// captured bodies in Fixtures/ are business notifications from
// plow-messages' own suite, with no name, address, amount or code.

import Foundation
import SQLite3
import Testing
import TypedStream

@testable import MessagesReader

/// What Messages writes in `attributedBody`: NSArchiver's typedstream.
/// The tests archive with it; the reader never unarchives.
func archived(_ text: String, attributes: [NSAttributedString.Key: Any] = [:]) -> [UInt8] {
    [UInt8](NSArchiver.archivedData(withRootObject: NSAttributedString(string: text, attributes: attributes)))
}

func archivedMutable(_ text: String) -> [UInt8] {
    [UInt8](NSArchiver.archivedData(withRootObject: NSMutableAttributedString(string: text)))
}

/// A body captured from a real database, as hex.
func captured(_ name: String) -> [UInt8] {
    let url = Bundle.module.url(forResource: "Fixtures/\(name)", withExtension: "hex")!
    let hex = try! String(contentsOf: url, encoding: .utf8).trimmingCharacters(in: .whitespacesAndNewlines)
    var bytes: [UInt8] = []
    var index = hex.startIndex
    while index < hex.endIndex {
        let next = hex.index(index, offsetBy: 2)
        bytes.append(UInt8(hex[index..<next], radix: 16)!)
        index = next
    }
    return bytes
}

let header: [UInt8] = [4, 11] + Array("streamtyped".utf8) + [0x81, 0xe8, 0x03]

/// A length as typedstream frames it: one byte, or 0x81 and two bytes, or
/// 0x82 and four.
func length(_ count: Int, littleEndian: Bool = true) -> [UInt8] {
    guard count >= 0x80 else { return [UInt8(count)] }
    let width = count < 0x10000 ? 2 : 4
    let bytes = (0..<width).map { UInt8(truncatingIfNeeded: count >> ($0 * 8)) }
    return [width == 2 ? 0x81 : 0x82] + (littleEndian ? bytes : bytes.reversed())
}

/// An archive as NSArchiver writes one: an NSString holding `text`, with
/// `inside` added inside it before it ends. Its tables then hold types
/// `@` (0x92), `NSString` (0x93) and `+` (0x94), and objects the string
/// (0x92) and its class (0x93).
func archive(_ text: String, inside: [UInt8] = [], end: Bool = true, littleEndian: Bool = true) -> [UInt8] {
    let signature = littleEndian ? "streamtyped" : "typedstream"
    let version: [UInt8] = littleEndian ? [0x81, 0xe8, 0x03] : [0x81, 0x03, 0xe8]
    // Appended a part at a time: one long `+` chain is more than some
    // compilers will type-check.
    var bytes: [UInt8] = [4, 11]
    bytes += Array(signature.utf8)
    bytes += version
    bytes += [0x84, 0x01, 0x40, 0x84, 0x84, 0x84, 0x08] as [UInt8]
    bytes += Array("NSString".utf8)
    bytes += [0x01, 0x85, 0x84, 0x01, 0x2b] as [UInt8]
    bytes += length(text.utf8.count, littleEndian: littleEndian)
    bytes += Array(text.utf8)
    bytes += inside
    if end { bytes.append(0x86) }
    return bytes
}

/// `count` objects inside one another, each an NSString with no contents.
func nested(_ count: Int) -> [UInt8] {
    let opened: [UInt8] = Array([[UInt8]](repeating: [0x92, 0x84, 0x93], count: count).joined())
    let ended: [UInt8] = [UInt8](repeating: 0x86, count: count)
    return opened + ended
}

/// Bodies that break each limit, each after its text, where a scan for the
/// first string would stop.
enum Malformed {
    /// Over 1 MiB.
    static let oversized = archive(String(repeating: "a", count: 1 << 20))
    /// 65 objects open inside the text's: 66 at once.
    static let deep = archive("hello", inside: nested(65))
    /// A second string whose length runs far past the end.
    static let lying = archive("short", inside: [0x94, 0x81, 0xff, 0x7f] + Array("tail".utf8))
    /// Long enough to outlast a small budget: a thousand short strings.
    static let slow: [UInt8] = {
        let string: [UInt8] = [0x94, 0x7f] + [UInt8](repeating: 0x61, count: 127)
        let strings: [UInt8] = Array([[UInt8]](repeating: string, count: 7000).joined())
        return archive("slow", inside: strings)
    }()
}

/// Nanoseconds since 2001, `secondsAgo` before `now`, as `message.date` holds them.
func date(_ secondsAgo: Double, now: Date) -> Int64 {
    Int64((now.timeIntervalSince1970 - secondsAgo - 978_307_200) * 1_000_000_000)
}

/// A chat.db and an address book under the repo's gitignored .dev/,
/// removed when dropped.
final class Fixture {
    let directory: URL
    let path: String
    let addressBook: String
    private var db: OpaquePointer?

    init() {
        let repo = URL(fileURLWithPath: #filePath).deletingLastPathComponent()
            .appendingPathComponent("../../../..").standardized
        directory = repo.appendingPathComponent(".dev/tests/messages-\(UUID().uuidString)")
        try! FileManager.default.createDirectory(at: directory, withIntermediateDirectories: true)
        path = directory.appendingPathComponent("chat.db").path
        addressBook = directory.appendingPathComponent("AddressBook").path
        precondition(sqlite3_open(path, &db) == SQLITE_OK)
        execute("""
            create table handle (ROWID integer primary key, id text, service text);
            create table chat (ROWID integer primary key, guid text, chat_identifier text,
                               display_name text, style integer, service_name text);
            create table message (ROWID integer primary key, guid text, text text,
                                  attributedBody blob, handle_id integer, date integer,
                                  is_from_me integer default 0,
                                  associated_message_type integer default 0,
                                  item_type integer default 0, service text);
            create table chat_message_join (chat_id integer, message_id integer, message_date integer);
            create table chat_handle_join (chat_id integer, handle_id integer);
            create table message_attachment_join (message_id integer, attachment_id integer);
            """)
    }

    /// An address book with `people`: name, nickname, organization, phones,
    /// emails. Written as the v22 store, in `Sources/<id>/` when `source` is set.
    func book(_ people: [(String?, String?, String?, [String], [String])], source: String? = nil) {
        var folder = URL(fileURLWithPath: addressBook)
        if let source { folder = folder.appendingPathComponent("Sources/\(source)") }
        try! FileManager.default.createDirectory(at: folder, withIntermediateDirectories: true)
        var book: OpaquePointer?
        precondition(sqlite3_open(folder.appendingPathComponent("AddressBook-v22.abcddb").path, &book) == SQLITE_OK)
        defer { sqlite3_close(book) }
        var sql = """
            create table ZABCDRECORD (Z_PK integer primary key, ZNICKNAME text, ZFIRSTNAME text,
                                      ZLASTNAME text, ZORGANIZATION text);
            create table ZABCDPHONENUMBER (Z_PK integer primary key, ZOWNER integer, ZFULLNUMBER text);
            create table ZABCDEMAILADDRESS (Z_PK integer primary key, ZOWNER integer, ZADDRESS text);
            """
        func quoted(_ text: String?) -> String { text.map { "'\($0)'" } ?? "NULL" }
        for (index, person) in people.enumerated() {
            let (name, nickname, organization, phones, emails) = person
            let parts = name?.split(separator: " ").map(String.init) ?? []
            sql += """
                insert into ZABCDRECORD values (\(index + 1), \(quoted(nickname)), \(quoted(parts.first)),
                    \(quoted(parts.dropFirst().joined(separator: " ").isEmpty ? nil : parts.dropFirst().joined(separator: " "))),
                    \(quoted(organization)));
                """
            for phone in phones { sql += "insert into ZABCDPHONENUMBER (ZOWNER, ZFULLNUMBER) values (\(index + 1), '\(phone)');" }
            for email in emails { sql += "insert into ZABCDEMAILADDRESS (ZOWNER, ZADDRESS) values (\(index + 1), '\(email)');" }
        }
        precondition(sqlite3_exec(book, sql, nil, nil, nil) == SQLITE_OK, String(cString: sqlite3_errmsg(book)))
    }

    deinit {
        sqlite3_close(db)
        try? FileManager.default.removeItem(at: directory)
    }

    func execute(_ sql: String) {
        precondition(sqlite3_exec(db, sql, nil, nil, nil) == SQLITE_OK, String(cString: sqlite3_errmsg(db)))
    }

    /// A chat with the handles (by handle id) in it.
    func chat(
        _ id: Int64, _ identifier: String, group: Bool = false, name: String? = nil, members: [Int64] = [],
        service: String = "iMessage"
    ) {
        let name = name.map { "'\($0)'" } ?? "NULL"
        execute("""
            insert into chat (ROWID, guid, chat_identifier, display_name, style, service_name)
            values (\(id), '\(service);-;\(identifier)', '\(identifier)', \(name), \(group ? 43 : 45), '\(service)')
            """)
        for member in members { execute("insert into chat_handle_join values (\(id), \(member))") }
    }

    func handle(_ id: Int64, _ address: String) {
        execute("insert into handle (ROWID, id) values (\(id), '\(address)')")
    }

    /// A message in `chat`, from `handle` (or from the owner), with `text`
    /// and or `body` (an attributedBody blob).
    func message(
        _ rowid: Int64, chat: Int64, handle: Int64?, at: Int64, text: String? = nil, body: [UInt8]? = nil,
        fromMe: Bool = false, tapback: Bool = false, notice: Bool = false, attachments: Int = 0
    ) {
        var stmt: OpaquePointer?
        let sql = """
            insert into message (ROWID, handle_id, date, text, attributedBody, is_from_me,
                                 associated_message_type, item_type, service)
            values (?, ?, ?, ?, ?, ?, ?, ?, 'iMessage')
            """
        precondition(sqlite3_prepare_v2(db, sql, -1, &stmt, nil) == SQLITE_OK)
        defer { sqlite3_finalize(stmt) }
        let transient = unsafeBitCast(-1, to: sqlite3_destructor_type.self)
        sqlite3_bind_int64(stmt, 1, rowid)
        if let handle { sqlite3_bind_int64(stmt, 2, handle) } else { sqlite3_bind_null(stmt, 2) }
        sqlite3_bind_int64(stmt, 3, at)
        if let text { sqlite3_bind_text(stmt, 4, text, -1, transient) } else { sqlite3_bind_null(stmt, 4) }
        if let body {
            _ = body.withUnsafeBytes { sqlite3_bind_blob(stmt, 5, $0.baseAddress, Int32($0.count), transient) }
        } else {
            sqlite3_bind_null(stmt, 5)
        }
        sqlite3_bind_int(stmt, 6, fromMe ? 1 : 0)
        sqlite3_bind_int(stmt, 7, tapback ? 2000 : 0)
        sqlite3_bind_int(stmt, 8, notice ? 1 : 0)
        precondition(sqlite3_step(stmt) == SQLITE_DONE, String(cString: sqlite3_errmsg(db)))
        execute("insert into chat_message_join (chat_id, message_id) values (\(chat), \(rowid))")
        for n in 0..<attachments { execute("insert into message_attachment_join values (\(rowid), \(n))") }
    }
}

/// What a call printed: its answer, its stderr and exit code.
struct Ran {
    let answer: [String: Any]
    let errors: [String]
    let exit: Int32

    var rows: [[String: Any]] { (answer["messages"] ?? answer["chats"] ?? answer["people"] ?? answer["handles"]) as? [[String: Any]] ?? [] }
    var texts: [String?] { rows.map { $0["text"] as? String } }
    var ids: [Int64] { rows.map { ($0["id"] as? NSNumber)?.int64Value ?? -1 } }
    var next: String? { answer["next"] as? String }
    var error: [String: Any]? {
        errors.first.flatMap { try? JSONSerialization.jsonObject(with: Data($0.utf8)) as? [String: Any] }?["error"]
            as? [String: Any]
    }
}

func read(_ fixture: Fixture, _ arguments: [String], now: Date = Date(), limits: Limits = Limits()) -> Ran {
    var lines: [String] = []
    var errors: [String] = []
    let exit = run(
        ["--store", fixture.path, "--addressbook", fixture.addressBook, "--region", "US"] + arguments, now: now,
        limits: limits, write: { lines.append($0) }, fail: { errors.append($0) })
    let answer = lines.first.flatMap { try? JSONSerialization.jsonObject(with: Data($0.utf8)) as? [String: Any] } ?? [:]
    return Ran(answer: answer, errors: errors, exit: exit)
}
