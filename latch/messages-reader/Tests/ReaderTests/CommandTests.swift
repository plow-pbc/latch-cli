// Every command against a synthetic chat.db and address book.

import Foundation
import Testing
import TypedStream

@testable import MessagesReader

let now = Date()

/// Direct chats with Ann (1) and Bo (2), a group (3) both post in, and the
/// shapes a real database holds: bodies only in attributedBody, tapbacks,
/// join notices. The address book knows Ann by a formatted number, Bo by
/// email, and two cards share Cy's number.
func everyday() -> Fixture {
    let f = Fixture()
    f.handle(10, "+15550000001")
    f.handle(20, "bo@example.com")
    f.handle(40, "+15550000004")
    f.chat(1, "+15550000001", members: [10])
    f.chat(2, "bo@example.com", members: [20])
    f.chat(3, "chat333", group: true, name: "Dinner", members: [10, 20, 40])
    f.message(101, chat: 1, handle: 10, at: date(7200, now: now), text: "lunch?")
    f.message(102, chat: 1, handle: nil, at: date(7000, now: now), text: "sure", fromMe: true)
    f.message(103, chat: 1, handle: 10, at: date(600, now: now), body: archived("Your code is 482913"), attachments: 1)
    f.message(104, chat: 1, handle: nil, at: date(500, now: now), fromMe: true, tapback: true)
    f.message(201, chat: 2, handle: 20, at: date(3000, now: now), body: captured("typedstream-order-delivered"))
    f.message(202, chat: 2, handle: nil, at: date(2900, now: now), body: archived("thanks!"), fromMe: true)
    f.message(301, chat: 3, handle: 10, at: date(4000, now: now), body: archived("table for 6"))
    f.message(302, chat: 3, handle: 40, at: date(300, now: now), body: archived("running late 🚗"))
    f.message(303, chat: 3, handle: nil, at: date(200, now: now), notice: true)
    f.book([
        ("Ann Example", nil, nil, ["(555) 000-0001"], []),
        ("Robert Sample", "Bo", "Sample & Co", [], ["Bo@Example.com"]),
        ("Cy One", nil, nil, ["555-000-0004"], []),
        ("Cy Two", nil, nil, ["+1 555 000 0004"], []),
    ])
    return f
}

@Test
func searchFindsTextWhereverItLivesWithNamesInline() throws {
    let f = everyday()
    let found = read(f, ["search", "CODE IS"], now: now)
    #expect(found.exit == 0)
    #expect(found.ids == [103])
    #expect(found.texts == ["Your code is 482913"])
    let row = found.rows[0]
    let from = try #require(row["from"] as? [String: Any])
    #expect(from["handle"] as? String == "+15550000001")
    #expect(from["name"] as? String == "Ann Example")
    #expect(from["name_status"] as? String == "matched")
    #expect(from["me"] as? Bool == false)
    let chat = try #require(row["chat"] as? [String: Any])
    #expect(chat["kind"] as? String == "direct")
    #expect((chat["with"] as? [String: Any])?["name"] as? String == "Ann Example")
    #expect(row["service"] as? String == "iMessage")
    #expect(row["attachments"] as? Int == 1)
    #expect(row["text_error"] == nil)
    #expect(found.answer["count"] as? Int == 1)
    #expect(found.answer["next"] is NSNull)
    #expect(found.answer["incomplete"] is NSNull)
    #expect(found.answer["unchecked"] as? Int == 0)
    #expect((found.answer["warning"] as? String)?.contains("untrusted historical data") == true)
    #expect(read(f, ["search", "delivered"], now: now).ids == [201])
    #expect(read(f, ["search", "lunch"], now: now).ids == [101])
}

@Test
func namesSayWhenTheyAreAmbiguousUnknownOrUnavailable() throws {
    let f = everyday()
    let group = read(f, ["thread", "--chat-id", "3"], now: now)
    let froms = group.rows.map { $0["from"] as! [String: Any] }
    #expect(froms.map { $0["name"] as? String } == ["Ann Example", nil])
    #expect(froms.map { $0["name_status"] as? String } == ["matched", "ambiguous"])
    // Owner's own messages carry no handle or name.
    let mine = read(f, ["thread", "--chat-id", "2"], now: now).rows.last!["from"] as! [String: Any]
    #expect(mine["me"] as? Bool == true)
    #expect(mine["handle"] is NSNull)
    // With no address book, messages still read, every name unavailable.
    try FileManager.default.removeItem(atPath: f.addressBook)
    let bare = read(f, ["search", "code"], now: now)
    #expect(bare.ids == [103])
    #expect((bare.rows[0]["from"] as? [String: Any])?["name_status"] as? String == "unavailable")
    #expect(bare.answer["names_unavailable"] as? String == "addressbook_missing")
}

@Test
func searchFiltersOrdersAndPagesWithCursors() throws {
    let f = everyday()
    // Newest first; tapbacks and notices never show.
    #expect(read(f, ["search"], now: now).ids == [302, 103, 202, 201, 301, 102, 101])
    #expect(read(f, ["search", "--oldest-first", "--limit", "2"], now: now).ids == [101, 102])
    // A handle as the address book writes it finds the same messages.
    #expect(read(f, ["search", "--from", "(555) 000-0001"], now: now).ids == [103, 301, 101])
    #expect(read(f, ["search", "--from", "BO@example.com"], now: now).ids == [201])
    #expect(read(f, ["search", "--chat-id", "3"], now: now).ids == [302, 301])
    #expect(read(f, ["search", "--since", "20m"], now: now).ids == [302, 103])
    // Pages: every row once, in order, then the end.
    var seen: [Int64] = []
    var page = read(f, ["search", "--limit", "3"], now: now)
    seen += page.ids
    while let next = page.next {
        page = read(f, ["search", "--cursor", next, "--limit", "3"], now: now)
        seen += page.ids
    }
    #expect(seen == [302, 103, 202, 201, 301, 102, 101])
    // A cursor keeps its filters, and is only for its own command.
    let filtered = read(f, ["search", "--from", "+15550000001", "--limit", "1"], now: now)
    #expect(filtered.ids == [103])
    #expect(read(f, ["search", "--cursor", filtered.next!], now: now).ids == [301, 101])
    #expect(read(f, ["search", "--cursor", filtered.next!, "--chat-id", "1"], now: now).exit == 2)
    #expect(read(f, ["chats", "--cursor", filtered.next!], now: now).exit == 2)
    #expect(read(f, ["search", "--cursor", "not-a-cursor"], now: now).exit == 2)
}

@Test
func threadReadsOneConversationOldestFirstAndPagesBack() throws {
    let f = everyday()
    #expect(read(f, ["thread", "--chat-id", "1"], now: now).ids == [101, 102, 103])
    let newest = read(f, ["thread", "--chat-id", "1", "--limit", "2"], now: now)
    #expect(newest.ids == [102, 103])
    #expect(read(f, ["thread", "--cursor", newest.next!], now: now).ids == [101])
    // --with reads the one direct chat with that handle, as any format names it.
    #expect(read(f, ["thread", "--with", "(555) 000-0001"], now: now).ids == [101, 102, 103])
    #expect(read(f, ["thread", "--with", "bo@example.com"], now: now).texts == [
        "Your order was delivered! Thank you for ordering from Super Duper Burgers.", "thanks!",
    ])
    #expect(read(f, ["thread", "--with", "a", "--with", "b"], now: now).exit == 2)
    #expect(read(f, ["thread"], now: now).exit == 2)
    #expect(read(f, ["thread", "--with", "+15559999999"], now: now).error?["code"] as? String == "no_such_chat")
    // Two direct chats with one person (iMessage and SMS) are named, not merged.
    f.chat(5, "+15550000001", members: [10], service: "SMS")
    f.message(501, chat: 5, handle: 10, at: date(100, now: now), text: "sms")
    let ambiguous = read(f, ["thread", "--with", "+15550000001"], now: now)
    #expect(ambiguous.exit == 2)
    #expect(ambiguous.error?["code"] as? String == "ambiguous_chat")
    #expect((ambiguous.error?["chats"] as? [[String: Any]])?.map { $0["id"] as? Int } == [1, 5])
}

@Test
func chatsListParticipantsWithNamesByLastRealMessage() throws {
    let f = everyday()
    let chats = read(f, ["chats"], now: now)
    #expect(chats.ids == [3, 1, 2])
    #expect(chats.rows.map { $0["kind"] as? String } == ["group", "direct", "direct"])
    let people = try #require(chats.rows[0]["participants"] as? [[String: Any]])
    #expect(people.map { $0["handle"] as? String } == ["+15550000001", "bo@example.com", "+15550000004"])
    #expect(people.map { $0["name"] as? String } == ["Ann Example", "Robert Sample", nil])
    #expect(read(f, ["chats", "--kind", "group"], now: now).ids == [3])
    let first = read(f, ["chats", "--limit", "2"], now: now)
    #expect(first.ids == [3, 1])
    #expect(read(f, ["chats", "--cursor", first.next!], now: now).ids == [2])
}

@Test
func unrepliedIsDirectChatsWhoseLastRealMessageIsInbound() {
    let f = everyday()
    // Ann's code is unanswered (a tapback is no reply); Bo was answered;
    // a group is not a direct chat.
    #expect(read(f, ["unreplied"], now: now).ids == [103])
    #expect(read(f, ["unreplied", "--within", "5m"], now: now).ids == [])
    #expect(read(f, ["unreplied"], now: now.addingTimeInterval(40 * 3600)).ids == [])
    #expect(read(f, ["unreplied", "--within", "8d"], now: now).exit == 2)
}

@Test
func limitsAndBadArgumentsAreUsageErrors() {
    let f = everyday()
    for n in 0..<60 {
        f.message(1000 + Int64(n), chat: 2, handle: 20, at: date(10_000 + Double(n), now: now), text: "old \(n)")
    }
    #expect(read(f, ["search"], now: now).rows.count == 50)
    #expect(read(f, ["search", "--limit", "500"], now: now).rows.count == 67)
    for bad in [
        ["search", "--limit", "501"], ["search", "--limit", "0"], ["search", "--kind", "group"],
        ["search", "--store", "/elsewhere"], ["search", "a", "b"], ["chats", "phrase"], ["send"],
        ["search", "--since", "yesterday"], ["contacts"], ["contacts", "search"],
    ] {
        let ran = read(f, bad, now: now)
        #expect(ran.exit == 2, "\(bad)")
        #expect(ran.error?["exit"] as? Int == 2, "\(bad)")
    }
}

@Test
func aBodyThatBreaksALimitIsAnErrorForThatRowAlone() {
    let f = everyday()
    f.handle(30, "+15550000003")
    f.chat(4, "+15550000003", members: [30])
    let bad: [(Int64, [UInt8])] = [
        (401, Malformed.oversized), (402, Malformed.deep), (403, Malformed.lying), (404, Malformed.slow),
    ]
    for (rowid, body) in bad {
        f.message(rowid, chat: 4, handle: 30, at: date(Double(5000 + rowid), now: now), body: body)
    }
    // A slow body is caught by its budget; give a small one to show that.
    let thread = read(f, ["thread", "--chat-id", "4"], now: now, limits: Limits(budget: .microseconds(100)))
    #expect(thread.exit == 0)
    #expect(thread.ids == [404, 403, 402, 401])
    #expect(thread.rows.map { $0["text_error"] as? String }
        == ["body_too_slow", "body_bad_length", "body_too_deep", "body_too_large"])
    #expect(thread.rows.allSatisfy { $0["text"] is NSNull })
    #expect(thread.answer["unreadable"] as? Int == 4)
    // The rest still reads beside them.
    let all = read(f, ["search", "--limit", "500"], now: now)
    #expect(all.rows.count == 11)
    // With the usual budget, the slow body reads.
    #expect(all.answer["unreadable"] as? Int == 3)
    #expect(all.texts.contains("Your code is 482913"))
    // In a phrase search, an unreadable body is a row that could not be
    // checked, not a match: every one is returned, and counted.
    let hello = read(f, ["search", "hello"], now: now)
    #expect(hello.ids == [401, 402, 403])
    #expect(hello.answer["unchecked"] as? Int == 3)
    // A row with text to fall back on shows that text.
    f.message(405, chat: 4, handle: 30, at: date(1, now: now), text: "fallback", body: Malformed.deep)
    #expect(read(f, ["thread", "--chat-id", "4", "--limit", "1"], now: now).texts == ["fallback"])
    // A plain text column over 1 MiB is refused like a body.
    f.message(406, chat: 4, handle: 30, at: date(0.5, now: now), text: String(repeating: "t", count: (1 << 20) + 1))
    #expect(read(f, ["thread", "--chat-id", "4", "--limit", "1"], now: now).rows[0]["text_error"] as? String
        == "body_too_large")
}

@Test
func anAnswerStaysUnderItsSizeBudgetAndSaysWhereToGoOn() throws {
    let f = everyday()
    // Twenty near-megabyte bodies: more than one answer may carry.
    let big = String(repeating: "x", count: 900_000)
    for n in 0..<20 {
        f.message(2000 + Int64(n), chat: 2, handle: 20, at: date(20_000 + Double(n), now: now), body: archived(big))
    }
    var seen = 0
    var pages = 0
    // Time enough for each big body, however busy the machine.
    let roomy = Limits(budget: .seconds(5))
    var page = read(f, ["search", "--from", "bo@example.com", "--limit", "500"], now: now, limits: roomy)
    while true {
        pages += 1
        let size = try JSONSerialization.data(withJSONObject: page.answer).count
        #expect(size < 8 << 20)
        seen += page.rows.count
        guard let next = page.next else { break }
        #expect(page.answer["incomplete"] as? String == "size_budget")
        page = read(f, ["search", "--cursor", next, "--limit", "500"], now: now, limits: roomy)
    }
    #expect(seen == 21)
    #expect(pages >= 3)
}

@Test
func aCallPastItsDeadlineStopsAndSaysWhereToGoOn() throws {
    let f = everyday()
    let store = try Store(path: f.path, deadline: ContinuousClock.now - .seconds(1))
    var call = Call()
    call.command = "search"
    call.addressBook = f.addressBook
    let context = Context(call: call, store: store, book: AddressBook(directory: f.addressBook, phones: Phones(region: "US")), limits: Limits())
    let answer = try context.search()
    #expect(store.interrupted)
    #expect(answer.incomplete == .timeBudget)
    #expect(answer.rows.count <= 1)
}

@Test
func contactsSearchAndLookupGiveHandlesToPassBack() throws {
    let f = everyday()
    let sample = read(f, ["contacts", "search", "sample"], now: now)
    #expect(sample.rows.map { $0["name"] as? String } == ["Robert Sample"])
    #expect(sample.rows[0]["nickname"] as? String == "Bo")
    // The full name, the nickname and the organization are each searched.
    for text in ["robert", "bo", "& co"] {
        #expect(read(f, ["contacts", "search", text], now: now).rows.map { $0["name"] as? String }
            .contains("Robert Sample"), "\(text)")
    }
    #expect(sample.rows[0]["organization"] as? String == "Sample & Co")
    #expect((sample.rows[0]["emails"] as? [[String: String]]) == [["display": "Bo@Example.com", "handle": "bo@example.com"]])
    let ann = read(f, ["contacts", "search", "ANN"], now: now)
    #expect((ann.rows[0]["phones"] as? [[String: String]]) == [["display": "(555) 000-0001", "handle": "+15550000001"]])
    #expect((ann.answer["warning"] as? String)?.contains("untrusted") == true)
    // Pages over the sorted names.
    let first = read(f, ["contacts", "search", "y", "--limit", "1"], now: now)
    #expect(first.rows.map { $0["name"] as? String } == ["Cy One"])
    #expect(read(f, ["contacts", "search", "--cursor", first.next!], now: now).rows.map { $0["name"] as? String }
        == ["Cy Two"])
    let lookup = read(f, ["contacts", "lookup", "+15550000001", "+15550000004", "x@y.z"], now: now)
    #expect(lookup.rows.map { $0["names"] as? [String] } == [["Ann Example"], ["Cy One", "Cy Two"], []])
    #expect(lookup.rows.map { $0["name_status"] as? String } == ["matched", "ambiguous", "none"])
}

@Test
func contactsSearchFindsEveryWordOnItsOwn() {
    let f = Fixture()
    f.book([
        ("Robert Smith", "Bob", nil, ["555-000-0011"], []),
        ("John Quincy Adams", nil, nil, ["555-000-0012"], []),
        ("José Núñez", nil, nil, ["555-000-0013"], []),
        ("Anna Kowalski", nil, nil, ["555-000-0014"], []),
    ])
    func names(_ text: String) -> [String?] {
        read(f, ["contacts", "search", text], now: now).rows.map { $0["name"] as? String }
    }
    // A word may be in the name or the nickname, in any order, past a middle
    // name, with or without its accents.
    #expect(names("Bob Smith") == ["Robert Smith"])
    #expect(names("john adams") == ["John Quincy Adams"])
    #expect(names("jose nunez") == ["José Núñez"])
    #expect(names("NÚÑEZ") == ["José Núñez"])
    // Every word must be there: a misspelt surname misses, and the first
    // name alone finds the card.
    #expect(names("robert jones") == [])
    #expect(names("anna kowalksi") == [])
    #expect(names("anna") == ["Anna Kowalski"])
}

@Test
func everyAccountsAddressBookIsRead() {
    let f = everyday()
    f.book([("Di Account", nil, nil, ["+44 20 7946 0000"], [])], source: "ABCD-1234")
    #expect(read(f, ["contacts", "lookup", "+442079460000"], now: now).rows.first?["names"] as? [String] == ["Di Account"])
    // A UK number written the national way matches only with the UK as region.
    var lines: [String] = []
    _ = run(["--store", f.path, "--addressbook", f.addressBook, "--region", "GB", "contacts", "lookup", "020 7946 0000"],
            write: { lines.append($0) }, fail: { _ in })
    #expect(lines.first?.contains("Di Account") == true)
    // An address book from an older system is refused, not read stale.
    f.book([("Old", nil, nil, [], [])], source: "OLD")
    try! FileManager.default.moveItem(
        atPath: f.addressBook + "/Sources/OLD/AddressBook-v22.abcddb",
        toPath: f.addressBook + "/Sources/OLD/AddressBook-v23.abcddb")
    #expect(read(f, ["contacts", "lookup", "x"], now: now).error?["code"] as? String == "addressbook_version")
}

@Test
func aMissingDatabaseOrAddressBookIsAnError() throws {
    var errors: [String] = []
    let exit = run(["--store", "/nonexistent/chat.db", "--addressbook", "/nonexistent", "chats"],
                   write: { _ in }, fail: { errors.append($0) })
    #expect(exit == 1)
    #expect(errors.first?.contains("messages_missing") == true)
    let f = everyday()
    try FileManager.default.removeItem(atPath: f.addressBook)
    #expect(read(f, ["contacts", "search", "a"], now: now).error?["code"] as? String == "addressbook_missing")
}

@Test
func theDatabaseIsOpenedReadOnly() throws {
    let f = everyday()
    let store = try Store(path: f.path, deadline: ContinuousClock.now + .seconds(10))
    #expect(throws: ReaderError.self) {
        try store.query("delete from message", []) { _ in true }
    }
    #expect(read(f, ["search"], now: now).rows.count == 7)
}

@Test
func wordsAfterADoubleDashMayStartWithADash() {
    let f = everyday()
    f.message(105, chat: 1, handle: 10, at: date(50, now: now), text: "-x marks it")
    #expect(read(f, ["search", "--", "-x"], now: now).ids == [105])
    #expect(read(f, ["search", "-x"], now: now).exit == 2)
}

@Test
func aDatabaseThePermissionsRefuseIsNotGranted() throws {
    let f = everyday()
    try FileManager.default.setAttributes([.posixPermissions: 0], ofItemAtPath: f.path)
    defer { try? FileManager.default.setAttributes([.posixPermissions: 0o600], ofItemAtPath: f.path) }
    #expect(read(f, ["chats"], now: now).error?["code"] as? String == "messages_not_granted")
}

@Test
func aPhraseSearchChecksEveryBodyAndCountsTheOnesItCannot() {
    let f = everyday()
    // UTF-16 bodies hold the phrase with a zero byte between letters.
    let utf16: [UInt8] = [0xff, 0xfe] + Array("Your code is 771205".utf16).flatMap { [UInt8($0 & 0xff), UInt8($0 >> 8)] }
    f.message(110, chat: 1, handle: 10, at: date(30, now: now), body: utf16)
    // A body that cannot be read, nothing in it the database could match.
    f.message(111, chat: 1, handle: 10, at: date(20, now: now), body: Malformed.lying)
    let found = read(f, ["search", "771205"], now: now)
    #expect(found.ids == [111, 110])
    #expect(found.texts[1] == "Your code is 771205")
    #expect(found.rows[0]["text_error"] as? String == "body_bad_length")
    #expect(found.answer["unchecked"] as? Int == 1)
    // A row with only a plain text that lacks the phrase is the one the database rules out.
    #expect(!read(f, ["search", "771205"], now: now).ids.contains(101))
}

/// What `plan` says a call would run.
func plan(_ fixture: Fixture, _ arguments: [String]) -> (Int32, [String: Any]) {
    var lines: [String] = []
    var errors: [String] = []
    let exit = run(["--region", "US", "plan"] + arguments, write: { lines.append($0) }, fail: { errors.append($0) })
    let text = lines.first ?? errors.first ?? "{}"
    return (exit, try! JSONSerialization.jsonObject(with: Data(text.utf8)) as! [String: Any])
}

@Test
func aPlanIsTheEffectiveRequestAndRunsAsIs() throws {
    let f = everyday()
    let (exit, typed) = plan(f, ["search", "--from", "(555) 000-0001", "--since", "2026-10-01T00:00:00Z", "--", "code"])
    #expect(exit == 0)
    let request = try #require(typed["plan"] as? [String: Any])
    #expect(request["command"] as? String == "search")
    #expect(request["from"] as? [String] == ["+15550000001"])
    #expect((request["since"] as? String)?.hasPrefix("2026-") == true)
    #expect(request["phrase"] as? String == "code")
    #expect(request["limit"] as? Int == 50)
    #expect(request["after"] == nil)
    // A later page's plan holds every filter its cursor carries, and where it starts.
    let first = read(f, ["search", "--from", "+15550000001", "--limit", "1"], now: now)
    let (_, paged) = plan(f, ["search", "--cursor", first.next!])
    let page = try #require(paged["plan"] as? [String: Any])
    #expect(page["from"] as? [String] == ["+15550000001"])
    #expect((page["after"] as? [String: Any])?["id"] as? Int == 103)
    // Running the plan gives that page.
    let data = try JSONSerialization.data(withJSONObject: page)
    var lines: [String] = []
    _ = run(["--store", f.path, "--addressbook", f.addressBook, "--plan", String(decoding: data, as: UTF8.self)],
            write: { lines.append($0) }, fail: { _ in })
    let answer = try #require(try JSONSerialization.jsonObject(with: Data(lines[0].utf8)) as? [String: Any])
    #expect((answer["messages"] as? [[String: Any]])?.map { $0["id"] as? Int } == [301, 101])
}

@Test
func aForgedCursorOrPlanIsCheckedLikeTypedFilters() throws {
    let f = everyday()
    // A thread cursor naming no conversation would read across them all.
    let forged = Cursor(command: "thread", filters: Filters(), key: String(date(0, now: now)), id: 999).token
    #expect(read(f, ["thread", "--cursor", forged], now: now).exit == 2)
    #expect(plan(f, ["thread", "--cursor", forged]).0 == 2)
    // Filters a command does not take, smuggled in a cursor.
    var filters = Filters()
    filters.chatId = 1
    filters.kind = "group"
    let mixed = Cursor(command: "thread", filters: filters, key: "1", id: 1).token
    #expect(read(f, ["thread", "--cursor", mixed], now: now).exit == 2)
    // A plan is checked as well.
    for bad in [#"{"command":"thread","limit":50}"#, #"{"command":"search","limit":900}"#,
                #"{"command":"chats","limit":5,"phrase":"x"}"#, "not json"] {
        var errors: [String] = []
        let exit = run(["--store", f.path, "--addressbook", f.addressBook, "--plan", bad], write: { _ in }, fail: { errors.append($0) })
        #expect(exit == 2, "\(bad)")
    }
}
