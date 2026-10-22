// The Messages database, opened read-only. Ported from plow-messages.

import Foundation
import SQLite3

/// Why a command could not run: a code, a message that holds no message
/// content, any details, and the exit code (latch's: 1 failed, 2 usage).
public struct ReaderError: Error, @unchecked Sendable {
    public let code: String
    public let message: String
    public let exit: Int32
    public var details: [String: Any] = [:]

    static func usage(_ message: String, code: String = "usage") -> ReaderError {
        ReaderError(code: code, message: message, exit: 2)
    }

    static func store(_ code: String, _ message: String) -> ReaderError {
        ReaderError(code: code, message: message, exit: 1)
    }
}

/// A read-only handle on chat.db. This reader has no write command.
final class Store {
    private var db: OpaquePointer?
    /// When the whole command must stop; a query running then is interrupted.
    private let deadline: ContinuousClock.Instant
    private(set) var interrupted = false

    init(path: String, deadline: ContinuousClock.Instant) throws {
        self.deadline = deadline
        var handle: OpaquePointer?
        // By path, not URI, so the path cannot carry `?mode=rw`. SQLite opens
        // the file lazily: reading the schema version opens it now, so a
        // missing file or a refused one is told apart here.
        let opened = sqlite3_open_v2(path, &handle, SQLITE_OPEN_READONLY, nil)
        var probe: OpaquePointer?
        let readable = opened == SQLITE_OK
            && sqlite3_prepare_v2(handle, "PRAGMA schema_version", -1, &probe, nil) == SQLITE_OK
            && sqlite3_step(probe) == SQLITE_ROW
        sqlite3_finalize(probe)
        guard readable, let handle else {
            sqlite3_close(handle)
            // SQLite does not always keep the system's reason; opening the
            // file plainly gets it.
            let fd = open(path, O_RDONLY)
            let errno = fd >= 0 ? 0 : Foundation.errno
            if fd >= 0 { close(fd) }
            switch errno {
            case EPERM, EACCES:
                throw ReaderError.store(
                    "messages_not_granted",
                    "the Messages database at \(path) cannot be read (permission refused); "
                        + "on a Mac this usually means the host lacks Full Disk Access")
            case ENOENT, ENOTDIR:
                throw ReaderError.store("messages_missing", "there is no Messages database at \(path)")
            default:
                throw ReaderError.store(
                    "messages_unreadable", "cannot read the Messages database at \(path) (errno \(errno))")
            }
        }
        db = handle
        // Every 1000 virtual-machine steps, stop a query past the deadline.
        sqlite3_progress_handler(
            handle, 1000,
            { context in
                let store = Unmanaged<Store>.fromOpaque(context!).takeUnretainedValue()
                guard ContinuousClock.now > store.deadline else { return 0 }
                store.interrupted = true
                return 1
            }, Unmanaged.passUnretained(self).toOpaque())
    }

    deinit {
        if let db { sqlite3_close(db) }
    }

    var pastDeadline: Bool { ContinuousClock.now > deadline }

    /// Runs `sql` with `params` bound in order, handing each row to `each`
    /// until it returns false. A query cut off at the deadline just ends;
    /// `interrupted` says so.
    func query(_ sql: String, _ params: [Param], _ each: (Row) -> Bool) throws {
        var stmt: OpaquePointer?
        guard sqlite3_prepare_v2(db, sql, -1, &stmt, nil) == SQLITE_OK, let stmt else {
            throw ReaderError.store("store_rejected", "the Messages database rejected a query (\(errorMessage))")
        }
        defer { sqlite3_finalize(stmt) }
        // SQLITE_TRANSIENT: sqlite copies the bytes.
        let transient = unsafeBitCast(-1, to: sqlite3_destructor_type.self)
        for (i, param) in params.enumerated() {
            switch param {
            case .text(let text): sqlite3_bind_text(stmt, Int32(i + 1), text, -1, transient)
            case .int(let n): sqlite3_bind_int64(stmt, Int32(i + 1), n)
            }
        }
        var step = sqlite3_step(stmt)
        while step == SQLITE_ROW {
            guard each(Row(stmt: stmt)) else { return }
            if pastDeadline {
                interrupted = true
                return
            }
            step = sqlite3_step(stmt)
        }
        if step == SQLITE_INTERRUPT { return }
        // A failed step (SQLITE_BUSY, say) is not the end of the rows.
        guard step == SQLITE_DONE else {
            throw ReaderError.store("store_failed", "reading the Messages database failed (\(errorMessage))")
        }
    }

    private var errorMessage: String { String(cString: sqlite3_errmsg(db)) }
}

enum Param {
    case text(String)
    case int(Int64)
}

/// One result row, read by column index.
struct Row {
    let stmt: OpaquePointer

    func int(_ i: Int32) -> Int64 { sqlite3_column_int64(stmt, i) }

    func string(_ i: Int32) -> String? {
        guard sqlite3_column_type(stmt, i) != SQLITE_NULL, let text = sqlite3_column_text(stmt, i) else {
            return nil
        }
        return String(cString: text)
    }

    /// A column's size in bytes, read without copying it.
    func size(_ i: Int32) -> Int { Int(sqlite3_column_bytes(stmt, i)) }

    /// A blob's bytes, or nil for NULL or empty.
    func blob(_ i: Int32) -> [UInt8]? {
        guard sqlite3_column_type(stmt, i) != SQLITE_NULL else { return nil }
        let count = Int(sqlite3_column_bytes(stmt, i))
        guard count > 0, let start = sqlite3_column_blob(stmt, i) else { return nil }
        return Array(UnsafeRawBufferPointer(start: start, count: count))
    }
}
