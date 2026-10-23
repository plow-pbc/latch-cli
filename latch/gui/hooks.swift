// hooks.sh's messenger to a test-hooks app (App/TestHooks.swift).
//   hooks post BUNDLE_ID COMMAND [ARG]   posts co.plow.latch.test.BUNDLE_ID.COMMAND, ARG its object
//   hooks onscreen PID                   prints how many windows PID has on screen
import CoreGraphics
import Foundation

let arguments = CommandLine.arguments
switch (arguments.count, arguments.dropFirst().first) {
case (4...5, "post"):
    // Delivered at once, even to an app that is not the active one.
    DistributedNotificationCenter.default().postNotificationName(
        .init("co.plow.latch.test.\(arguments[2]).\(arguments[3])"), object: arguments.count > 4 ? arguments[4] : nil,
        userInfo: nil, deliverImmediately: true)
case (3, "onscreen"):
    let windows = CGWindowListCopyWindowInfo([.optionOnScreenOnly], kCGNullWindowID) as? [[String: Any]] ?? []
    print(windows.filter { ($0[kCGWindowOwnerPID as String] as? Int).map(String.init) == arguments[2] }.count)
default:
    FileHandle.standardError.write(Data("usage: hooks post BUNDLE_ID COMMAND [ARG] | hooks onscreen PID\n".utf8))
    exit(2)
}
