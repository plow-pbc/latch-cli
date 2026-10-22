// The two Contacts commands: search the address book by name, and look up
// handles. Every phone and email comes with the canonical handle that
// `search --from` and `thread --with` take unchanged.

import Foundation

func book(_ call: Call) throws -> AddressBook {
    let book = AddressBook(directory: call.addressBook, phones: Phones(region: call.region))
    if let unavailable = book.unavailable {
        throw ReaderError.store(unavailable, "the address book cannot be read (\(unavailable))")
    }
    return book
}

func contactsSearch(_ call: Call, deadline: ContinuousClock.Instant) throws -> Answer {
    let book = try book(call)
    let text = (call.filters.text ?? "").lowercased()
    // Sorted by name, ties by their place in the book, so pages are stable
    // while the book is unchanged.
    let found = book.people.enumerated()
        .filter { _, person in
            [person.name, person.nickname, person.organization].contains { $0?.lowercased().contains(text) ?? false }
        }
        .sorted { ($0.element.name, $0.offset) < ($1.element.name, $1.offset) }
    var answer = Answer("people")
    let start = call.cursor.map { cursor in
        found.firstIndex { ($0.element.name, $0.offset) > (cursor.key, Int(cursor.id)) } ?? found.count
    } ?? 0
    for index in start..<found.count {
        let person = found[index].element
        if answer.rows.count == call.limit || ContinuousClock.now > deadline {
            if answer.rows.count < call.limit { answer.incomplete = .timeBudget }
            if index > start {
                let last = found[index - 1]
                answer.next = Cursor(
                    command: call.command, filters: call.filters, key: last.element.name, id: Int64(last.offset))
            } else {
                answer.next = call.cursor
            }
            break
        }
        let object: [String: Any] = [
            "name": person.name,
            "nickname": person.nickname ?? NSNull(),
            "organization": person.organization ?? NSNull(),
            "phones": person.phones.map { ["display": $0, "handle": book.phones.handle($0)] },
            "emails": person.emails.map { ["display": $0, "handle": book.phones.handle($0)] },
        ]
        guard answer.add(object) else {
            if index > start {
                let last = found[index - 1]
                answer.next = Cursor(
                    command: call.command, filters: call.filters, key: last.element.name, id: Int64(last.offset))
            } else {
                answer.next = call.cursor
            }
            break
        }
    }
    answer.extra["warning"] = untrusted
    return answer
}

func contactsLookup(_ call: Call) throws -> Answer {
    let book = try book(call)
    var answer = Answer("handles")
    for handle in call.filters.handles {
        let names = book.names(for: handle)
        let status: NameStatus = names.isEmpty ? .none : names.count == 1 ? .matched : .ambiguous
        _ = answer.add([
            "handle": handle,
            "canonical": book.phones.handle(handle),
            "names": names,
            "name_status": status.rawValue,
        ])
    }
    answer.extra["warning"] = untrusted
    return answer
}
