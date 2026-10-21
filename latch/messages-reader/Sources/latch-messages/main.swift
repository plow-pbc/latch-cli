// latch-messages: the read-only Messages reader a latch host runs for
// `latch messages`. See `latch-messages --help`.

import Foundation
import MessagesReader

let code = run(
    Array(CommandLine.arguments.dropFirst()),
    write: { print($0) },
    fail: { FileHandle.standardError.write(Data(($0 + "\n").utf8)) })
exit(code)
