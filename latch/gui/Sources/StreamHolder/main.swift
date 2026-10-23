// For the tests: holds latch streams as the app does, until killed:
// `StreamHolder LATCH STATE_DIR STREAM…`, each STREAM `status` or `audit`.
import Foundation
import PlowLatchUI

let arguments = Array(CommandLine.arguments.dropFirst())
await StreamHolder.run(latch: URL(filePath: arguments[0]), stateDir: arguments[1], streams: Array(arguments.dropFirst(2)))
