// The owner's address book, read from its SQLite stores in place. Ported
// from imsg's AddressBookContacts (openclaw/imsg 2040293, MIT, see
// LICENSE-imsg): the same files and query, without the Contacts framework,
// so reading it raises no Contacts prompt. It needs Full Disk Access, as the
// Messages database does. Read-only; never copied or changed.

import Foundation
import SQLite3

/// One card: who (the full name, else the nickname, else the
/// organization), and every phone and email on it.
struct Person {
    let name: String
    let nickname: String?
    let organization: String?
    var phones: [String] = []
    var emails: [String] = []
}

/// Whether a name could be given for a handle.
enum NameStatus: String {
    /// One name, from every card that has this handle.
    case matched
    /// No card has this handle.
    case none
    /// Cards with different names have it; none is chosen.
    case ambiguous
    /// The address book could not be read.
    case unavailable
}

final class AddressBook {
    let people: [Person]
    let unavailable: String?
    private var byHandle: [String: Set<String>] = [:]
    let phones: Phones

    /// Reads every store under `directory` (the main one and one per
    /// account in `Sources/`). A store that cannot be read makes the whole
    /// book unavailable, rather than quietly missing names.
    init(directory: String, phones: Phones) {
        self.phones = phones
        do {
            people = try AddressBook.load(directory)
            unavailable = nil
        } catch let error as ReaderError {
            people = []
            unavailable = error.code
        } catch {
            people = []
            unavailable = "addressbook_unreadable"
        }
        for person in people {
            for raw in person.phones + person.emails {
                byHandle[phones.handle(raw), default: []].insert(person.name)
            }
        }
    }

    /// The name for a Messages handle, and how sure that is.
    func name(for handle: String?) -> (String?, NameStatus) {
        guard unavailable == nil else { return (nil, .unavailable) }
        guard let handle, let names = byHandle[phones.handle(handle)] else { return (nil, .none) }
        guard names.count == 1 else { return (nil, .ambiguous) }
        return (names.first, .matched)
    }

    /// Every name on cards with this handle, sorted.
    func names(for handle: String) -> [String] {
        byHandle[phones.handle(handle)].map { $0.sorted() } ?? []
    }

    static func defaultDirectory() -> String {
        (NSHomeDirectory() as NSString).appendingPathComponent("Library/Application Support/AddressBook")
    }

    private static func load(_ directory: String) throws -> [Person] {
        let manager = FileManager.default
        func list(_ path: String) throws -> [String] {
            do {
                return try manager.contentsOfDirectory(atPath: path)
            } catch let error as NSError where error.code == NSFileReadNoPermissionError {
                throw ReaderError.store("addressbook_not_granted", "the address book at \(path) cannot be read")
            } catch {
                throw ReaderError.store("addressbook_missing", "there is no address book at \(path)")
            }
        }
        var folders = [directory]
        if try list(directory).contains("Sources") {
            let sources = (directory as NSString).appendingPathComponent("Sources")
            folders += try list(sources).sorted().map { (sources as NSString).appendingPathComponent($0) }
        }
        var stores: [String] = []
        for folder in folders {
            var isDirectory: ObjCBool = false
            guard manager.fileExists(atPath: folder, isDirectory: &isDirectory), isDirectory.boolValue else {
                continue
            }
            let versions = try list(folder).compactMap { name -> Int? in
                guard name.hasPrefix("AddressBook-v"), name.hasSuffix(".abcddb") else { return nil }
                return Int(name.dropFirst("AddressBook-v".count).dropLast(".abcddb".count))
            }
            guard let newest = versions.max() else { continue }
            // An OS upgrade can leave an old store behind; its names may be stale.
            guard newest == 22 else {
                throw ReaderError.store("addressbook_version", "the address book is version \(newest), not 22")
            }
            stores.append((folder as NSString).appendingPathComponent("AddressBook-v22.abcddb"))
        }
        guard !stores.isEmpty else {
            throw ReaderError.store("addressbook_missing", "there is no address book in \(directory)")
        }
        return try stores.flatMap(loadStore)
    }

    private static func loadStore(_ path: String) throws -> [Person] {
        var db: OpaquePointer?
        guard sqlite3_open_v2(path, &db, SQLITE_OPEN_READONLY, nil) == SQLITE_OK, let db else {
            sqlite3_close(db)
            throw ReaderError.store("addressbook_unreadable", "cannot open the address book store \(path)")
        }
        defer { sqlite3_close(db) }
        sqlite3_busy_timeout(db, 250)
        let organization = hasColumn(db, "ZABCDRECORD", "ZORGANIZATION") ? "r.ZORGANIZATION" : "NULL"
        let sql = """
            SELECT r.Z_PK, r.ZNICKNAME, r.ZFIRSTNAME, r.ZLASTNAME, \(organization), p.ZFULLNUMBER, NULL
              FROM ZABCDRECORD r JOIN ZABCDPHONENUMBER p ON p.ZOWNER = r.Z_PK
            UNION ALL
            SELECT r.Z_PK, r.ZNICKNAME, r.ZFIRSTNAME, r.ZLASTNAME, \(organization), NULL, e.ZADDRESS
              FROM ZABCDRECORD r JOIN ZABCDEMAILADDRESS e ON e.ZOWNER = r.Z_PK
            ORDER BY 1
            """
        var stmt: OpaquePointer?
        guard sqlite3_prepare_v2(db, sql, -1, &stmt, nil) == SQLITE_OK, let stmt else {
            throw ReaderError.store("addressbook_unreadable", "the address book store \(path) has another shape")
        }
        defer { sqlite3_finalize(stmt) }
        var people: [Int64: Person] = [:]
        var order: [Int64] = []
        var step = sqlite3_step(stmt)
        while step == SQLITE_ROW {
            let row = Row(stmt: stmt)
            let id = row.int(0)
            let nickname = row.string(1) ?? ""
            let full = [row.string(2), row.string(3)].compactMap { $0 }.filter { !$0.isEmpty }
                .joined(separator: " ")
            let organization = row.string(4).flatMap { $0.isEmpty ? nil : $0 }
            let name = !full.isEmpty ? full : !nickname.isEmpty ? nickname : organization ?? ""
            if !name.isEmpty {
                if people[id] == nil {
                    people[id] = Person(
                        name: name, nickname: nickname.isEmpty ? nil : nickname, organization: organization)
                    order.append(id)
                }
                if let phone = row.string(5), !phone.isEmpty { people[id]?.phones.append(phone) }
                if let email = row.string(6), !email.isEmpty { people[id]?.emails.append(email) }
            }
            step = sqlite3_step(stmt)
        }
        guard step == SQLITE_DONE else {
            throw ReaderError.store("addressbook_unreadable", "reading the address book store \(path) failed")
        }
        return order.compactMap { people[$0] }
    }

    private static func hasColumn(_ db: OpaquePointer, _ table: String, _ column: String) -> Bool {
        var stmt: OpaquePointer?
        guard sqlite3_prepare_v2(db, "PRAGMA table_info(\(table))", -1, &stmt, nil) == SQLITE_OK, let stmt else {
            return false
        }
        defer { sqlite3_finalize(stmt) }
        while sqlite3_step(stmt) == SQLITE_ROW {
            if Row(stmt: stmt).string(1) == column { return true }
        }
        return false
    }
}
