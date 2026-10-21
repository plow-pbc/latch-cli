// Helpers for scripts/measure.sh, compiled by it:
//   measure launch-ms APP_BINARY [ARGS...]   start the app, print ms from its
//                                          process start to its first window on
//                                          screen, then its pid
//   measure post COMMAND [OBJECT]            post co.plow.latch.test.COMMAND
// The window is found in the window server's list (no screen-recording
// permission needed for the list itself), so the app needs no timing code.
import AppKit
import CoreGraphics
import Darwin

func processStart(_ pid: pid_t) -> Date? {
    var info = kinfo_proc()
    var size = MemoryLayout<kinfo_proc>.stride
    var mib: [Int32] = [CTL_KERN, KERN_PROC, KERN_PROC_PID, pid]
    guard sysctl(&mib, 4, &info, &size, nil, 0) == 0, size > 0 else { return nil }
    let tv = info.kp_proc.p_un.__p_starttime
    return Date(timeIntervalSince1970: Double(tv.tv_sec) + Double(tv.tv_usec) / 1e6)
}

/// A normal-level window of `pid` that is on screen and drawn.
func hasVisibleWindow(_ pid: pid_t) -> Bool {
    let list = CGWindowListCopyWindowInfo([.optionOnScreenOnly], kCGNullWindowID) as? [[String: Any]] ?? []
    return list.contains { window in
        (window[kCGWindowOwnerPID as String] as? Int32) == pid
            && (window[kCGWindowLayer as String] as? Int) == 0
            && ((window[kCGWindowAlpha as String] as? Double) ?? 0) > 0
    }
}

let args = CommandLine.arguments
switch args.dropFirst().first {
case "launch-ms":
    let process = Process()
    process.executableURL = URL(filePath: args[2])
    process.arguments = Array(args.dropFirst(3))
    process.standardOutput = FileHandle.nullDevice
    process.standardError = FileHandle.nullDevice
    try process.run()
    let pid = process.processIdentifier
    let deadline = Date.now.addingTimeInterval(10)
    while !hasVisibleWindow(pid) {
        if Date.now > deadline { print("timeout \(pid)"); exit(1) }
        usleep(1000)
    }
    let seen = Date.now
    guard let start = processStart(pid) else { print("gone \(pid)"); exit(1) }
    print(String(format: "%.1f %d", seen.timeIntervalSince(start) * 1000, pid))
case "post":
    DistributedNotificationCenter.default().postNotificationName(
        .init("co.plow.latch.test.\(args[2])"), object: args.count > 3 ? args[3] : nil,
        userInfo: nil, deliverImmediately: true)
default:
    print("usage: measure launch-ms APP [ARGS...] | post COMMAND [OBJECT]")
    exit(2)
}
