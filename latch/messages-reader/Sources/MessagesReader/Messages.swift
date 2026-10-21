// The four Messages commands: search, thread, chats, unreplied. The queries
// are plow-messages'; bodies are decoded by TypedStream; names come from
// the address book.

import Foundation
import TypedStream

/// Apple's epoch: `message.date` counts nanoseconds from 2001-01-01.
let coreDataEpoch: Double = 978_307_200

/// `chat.style`: 43 is a group, 45 direct. `chat_identifier` is free text
/// and cannot tell them apart (`chatty@example.com` is direct).
let groupChatStyle = 43

/// A message's text column is held to the same size as its body.
let maxTextBytes = 1 << 20

/// Real messages only: a tapback has `associated_message_type != 0`, a
/// join or leave notice `item_type != 0`.
func realRows(_ alias: String) -> String {
    "\(alias).associated_message_type = 0 and \(alias).item_type = 0"
}

/// The columns every message read selects, in the order `messageRow` reads them.
let messageColumns = """
    select m.ROWID, c.ROWID, c.display_name, c.style, c.chat_identifier, h.id, m.is_from_me,
           m.date, m.text, m.attributedBody, m.service,
           (select count(*) from message_attachment_join a where a.message_id = m.ROWID)
      from message m
      join chat_message_join j on j.message_id = m.ROWID
      join chat c on c.ROWID = j.chat_id
      left join handle h on h.ROWID = m.handle_id
    """

func nanoseconds(_ seconds: Double) -> Int64 { Int64((seconds - coreDataEpoch) * 1_000_000_000) }
func seconds(_ nanoseconds: Int64) -> Double { Double(nanoseconds) / 1_000_000_000 + coreDataEpoch }

/// A message's text, or why it cannot be given.
func text(_ row: Row, _ limits: Limits) -> (String?, BodyError?) {
    // `attributedBody` first; older rows have only `text`. A body that cannot
    // be read falls back to `text`, and is an error without it.
    let plain: String? = row.size(8) > maxTextBytes ? nil : row.string(8)
    let plainTooLarge = row.size(8) > maxTextBytes
    var decoded: Result<String, BodyError>?
    if row.size(9) > limits.maxBytes {
        decoded = .failure(.tooLarge)
    } else if let blob = row.blob(9) {
        decoded = decodeAttributedBody(blob, limits: limits)
    }
    switch decoded {
    case .success(let body)?: return (body, nil)
    case .failure(let error)? where plain == nil: return (nil, plainTooLarge ? .tooLarge : error)
    default:
        if plainTooLarge { return (nil, .tooLarge) }
        return (plain, nil)
    }
}

/// Runs search, thread, chats or unreplied.
func messages(_ call: Call, now: Date, limits: Limits, deadline: ContinuousClock.Instant) throws -> Answer {
    let store = try Store(path: call.store, deadline: deadline)
    let phones = Phones(region: call.region)
    let book = AddressBook(directory: call.addressBook, phones: phones)
    let context = Context(call: call, store: store, book: book, limits: limits)
    var answer: Answer
    switch call.command {
    case "search": answer = try context.search()
    case "thread": answer = try context.thread()
    case "chats": answer = try context.chats()
    default: answer = try context.unreplied(now: now)
    }
    if store.interrupted && answer.incomplete == nil { answer.incomplete = .timeBudget }
    answer.extra["warning"] = untrusted
    if let unavailable = book.unavailable { answer.extra["names_unavailable"] = unavailable }
    return answer
}

struct Context {
    let call: Call
    let store: Store
    let book: AddressBook
    let limits: Limits
    var filters: Filters { call.filters }

    /// Someone in a chat: their handle, and their name if the address book
    /// gives exactly one.
    func person(_ handle: String?) -> [String: Any] {
        let (name, status) = book.name(for: handle)
        return [
            "handle": handle.map(book.phones.handle) ?? NSNull(),
            "name": name ?? NSNull(),
            "name_status": handle == nil ? NSNull() : status.rawValue,
        ]
    }

    /// One message as an answer row, and whether its text was unreadable.
    func messageRow(_ row: Row) -> ([String: Any], Bool) {
        let direct = row.int(3) != Int64(groupChatStyle)
        let me = row.int(6) == 1
        var from: [String: Any] = ["me": me]
        if me {
            from.merge(["handle": NSNull(), "name": NSNull(), "name_status": NSNull()]) { $1 }
        } else {
            from.merge(person(row.string(5))) { $1 }
        }
        let chat: [String: Any] = [
            "id": row.int(1),
            "kind": direct ? "direct" : "group",
            "name": row.string(2).flatMap { $0.isEmpty ? nil : $0 } ?? NSNull(),
            // A direct chat's other party, which an outbound message is to.
            "with": direct ? person(row.string(4)) : NSNull(),
        ]
        let (body, error) = text(row, limits)
        var object: [String: Any] = [
            "id": row.int(0),
            "at": iso(seconds(row.int(7))),
            "from": from,
            "chat": chat,
            "service": row.string(10) ?? NSNull(),
            "text": body ?? NSNull(),
            "attachments": row.int(11),
        ]
        if let error { object["text_error"] = error.rawValue }
        return (object, error != nil)
    }

    func cursor(_ key: Int64, _ id: Int64) -> Cursor {
        Cursor(command: call.command, filters: filters, key: String(key), id: id)
    }

    /// The position a cursor gives, as SQL and its parameters.
    func after(_ columns: String, descending: Bool) -> (String, [Param])? {
        guard let cursor = call.cursor, let key = Int64(cursor.key) else { return nil }
        return ("(\(columns)) \(descending ? "<" : ">") (?, ?)", [.int(key), .int(cursor.id)])
    }

    func search() throws -> Answer {
        var conditions = [realRows("m")]
        var params: [Param] = []
        let needle = asciiLower(filters.phrase ?? "")
        // A prefilter, not the decision: each row it passes is decoded and
        // checked below. A body that cannot be decoded stays a candidate.
        if !needle.isEmpty {
            conditions.append(
                "(instr(lower(cast(m.attributedBody as text)), lower(?)) > 0 or instr(lower(m.text), lower(?)) > 0)")
            params += [.text(needle), .text(needle)]
        }
        if !filters.from.isEmpty {
            let handles = Array(Set(filters.from.flatMap { [$0, book.phones.handle($0)] }))
            conditions.append("lower(h.id) in (\(handles.map { _ in "lower(?)" }.joined(separator: ",")))")
            params += handles.map(Param.text)
            conditions.append("m.is_from_me = 0")
        }
        if let chatId = filters.chatId {
            conditions.append("j.chat_id = ?")
            params.append(.int(chatId))
        }
        if let since = filters.since {
            conditions.append("m.date >= ?")
            params.append(.int(nanoseconds(since)))
        }
        if let until = filters.until {
            conditions.append("m.date <= ?")
            params.append(.int(nanoseconds(until)))
        }
        let descending = !filters.oldestFirst
        if let (sql, bound) = after("m.date, m.ROWID", descending: descending) {
            conditions.append(sql)
            params += bound
        }
        let direction = descending ? "desc" : "asc"
        let sql = messageColumns + " where " + conditions.joined(separator: " and ")
            + " order by m.date \(direction), m.ROWID \(direction)"
        var answer = Answer("messages")
        var unreadable = 0
        var last: (Int64, Int64)?
        var scanned: (Int64, Int64)?
        try store.query(sql, params) { row in
            scanned = (row.int(7), row.int(0))
            let (object, failed) = messageRow(row)
            let body = object["text"] as? String
            guard needle.isEmpty || failed || asciiLower(body ?? "").contains(needle) else { return true }
            // One more match than the limit: there is a next page.
            if answer.rows.count == call.limit {
                answer.next = cursor(last!.0, last!.1)
                return false
            }
            guard answer.add(object) else {
                answer.next = last.map { cursor($0.0, $0.1) } ?? call.cursor
                return false
            }
            if failed { unreadable += 1 }
            last = (row.int(7), row.int(0))
            return true
        }
        if store.interrupted {
            answer.incomplete = .timeBudget
            answer.next = scanned.map { cursor($0.0, $0.1) } ?? call.cursor
        }
        answer.extra["unreadable"] = unreadable
        // With a phrase, an unreadable body is a row that could not be
        // checked, not a match.
        if !needle.isEmpty { answer.extra["unchecked"] = unreadable }
        return answer
    }

    func thread() throws -> Answer {
        var conditions = [realRows("m")]
        var params: [Param] = []
        if let chatId = filters.chatId {
            conditions.append("j.chat_id = ?")
            params.append(.int(chatId))
        } else if let with = filters.with {
            conditions.append("j.chat_id = ?")
            params.append(.int(try directChat(with)))
        }
        if let (sql, bound) = after("m.date, m.ROWID", descending: true) {
            conditions.append(sql)
            params += bound
        }
        // The newest rows, read oldest first; the next page is older.
        let sql = messageColumns + " where " + conditions.joined(separator: " and ")
            + " order by m.date desc, m.ROWID desc limit ?"
        var answer = Answer("messages")
        var unreadable = 0
        var oldest: (Int64, Int64)?
        try store.query(sql, params + [.int(Int64(call.limit + 1))]) { row in
            if answer.rows.count == call.limit {
                answer.next = cursor(oldest!.0, oldest!.1)
                return false
            }
            let (object, failed) = messageRow(row)
            guard answer.add(object) else {
                answer.next = oldest.map { cursor($0.0, $0.1) } ?? call.cursor
                return false
            }
            if failed { unreadable += 1 }
            oldest = (row.int(7), row.int(0))
            return true
        }
        if store.interrupted { answer.next = oldest.map { cursor($0.0, $0.1) } ?? call.cursor }
        answer.rows.reverse()
        answer.extra["unreadable"] = unreadable
        return answer
    }

    /// The one direct chat with `handle`. Several (one per service, say)
    /// are named back for the caller to pick with --chat-id.
    func directChat(_ handle: String) throws -> Int64 {
        let wanted = book.phones.handle(handle)
        var matches: [[String: Any]] = []
        try store.query(
            "select ROWID, chat_identifier, service_name from chat where style != ? order by ROWID",
            [.int(Int64(groupChatStyle))]
        ) { row in
            if let identifier = row.string(1), book.phones.handle(identifier) == wanted {
                matches.append(["id": row.int(0), "service": row.string(2) ?? NSNull()])
            }
            return true
        }
        guard matches.count <= 1 else {
            var error = ReaderError.usage(
                "\(matches.count) direct chats are with \(wanted); pick one with --chat-id", code: "ambiguous_chat")
            error.details = ["chats": matches]
            throw error
        }
        guard let id = matches.first?["id"] as? Int64 else {
            throw ReaderError.usage("there is no direct chat with \(wanted)", code: "no_such_chat")
        }
        return id
    }

    func chats() throws -> Answer {
        var having: [String] = []
        var params: [Param] = []
        if let kind = filters.kind {
            having.append(kind == "group" ? "c.style = \(groupChatStyle)" : "c.style != \(groupChatStyle)")
        }
        if let (sql, bound) = after("max(m.date), c.ROWID", descending: true) {
            having.append(sql)
            params += bound
        }
        // Real rows only, or a chat's last tapback would be its last message.
        let sql = """
            select c.ROWID, c.display_name, c.style, c.service_name, max(m.date)
              from chat c
              join chat_message_join j on j.chat_id = c.ROWID
              join message m on m.ROWID = j.message_id
             where \(realRows("m"))
             group by c.ROWID
            \(having.isEmpty ? "" : "having " + having.joined(separator: " and "))
             order by max(m.date) desc, c.ROWID desc
             limit ?
            """
        var chats: [(Int64, String?, Bool, String?, Int64)] = []
        try store.query(sql, params + [.int(Int64(call.limit + 1))]) { row in
            chats.append((row.int(0), row.string(1), row.int(2) == Int64(groupChatStyle), row.string(3), row.int(4)))
            return true
        }
        var answer = Answer("chats")
        for (index, chat) in chats.enumerated() {
            if index == call.limit {
                let last = chats[index - 1]
                answer.next = cursor(last.4, last.0)
                break
            }
            let object: [String: Any] = [
                "id": chat.0,
                "kind": chat.2 ? "group" : "direct",
                "name": chat.1.flatMap { $0.isEmpty ? nil : $0 } ?? NSNull(),
                "participants": try participants(chat.0),
                "service": chat.3 ?? NSNull(),
                "last_message_at": iso(seconds(chat.4)),
            ]
            guard answer.add(object) else {
                answer.next = index == 0 ? call.cursor : cursor(chats[index - 1].4, chats[index - 1].0)
                break
            }
        }
        return answer
    }

    /// Everyone in a chat but the owner, each with a name if one is known.
    func participants(_ chat: Int64) throws -> [[String: Any]] {
        var people: [[String: Any]] = []
        try store.query(
            "select h.id from chat_handle_join ch join handle h on h.ROWID = ch.handle_id where ch.chat_id = ? order by h.ROWID",
            [.int(chat)]
        ) { row in
            people.append(person(row.string(0)))
            return true
        }
        return people
    }

    func unreplied(now: Date) throws -> Answer {
        // A direct chat whose newest real message is inbound and recent: a
        // mechanical list of candidates, not proof a reply is owed.
        let cutoff = nanoseconds(now.timeIntervalSince1970 - (filters.within ?? 36 * 3600))
        var conditions = [
            realRows("m"), "c.style != \(groupChatStyle)", "m.is_from_me = 0", "m.date > ?",
            """
            m.ROWID = (select m2.ROWID from message m2
                         join chat_message_join j2 on j2.message_id = m2.ROWID
                        where j2.chat_id = c.ROWID and \(realRows("m2"))
                        order by m2.date desc, m2.ROWID desc limit 1)
            """,
        ]
        var params: [Param] = [.int(cutoff)]
        if let (sql, bound) = after("m.date, m.ROWID", descending: true) {
            conditions.append(sql)
            params += bound
        }
        let sql = messageColumns + " where " + conditions.joined(separator: " and ")
            + " order by m.date desc, m.ROWID desc limit ?"
        var answer = Answer("messages")
        var unreadable = 0
        var last: (Int64, Int64)?
        try store.query(sql, params + [.int(Int64(call.limit + 1))]) { row in
            if answer.rows.count == call.limit {
                answer.next = cursor(last!.0, last!.1)
                return false
            }
            let (object, failed) = messageRow(row)
            guard answer.add(object) else {
                answer.next = last.map { cursor($0.0, $0.1) } ?? call.cursor
                return false
            }
            if failed { unreadable += 1 }
            last = (row.int(7), row.int(0))
            return true
        }
        if store.interrupted { answer.next = last.map { cursor($0.0, $0.1) } ?? call.cursor }
        answer.extra["unreadable"] = unreadable
        return answer
    }
}

/// ASCII letters folded, as SQLite's `lower()` folds them, and nothing else.
func asciiLower(_ text: String) -> String {
    String(String.UnicodeScalarView(text.unicodeScalars.map { scalar in
        (65...90).contains(scalar.value) ? Unicode.Scalar(scalar.value + 32)! : scalar
    }))
}
