// Draws Plow Latch.app's icon: the Plow lettermark in volt on the brand's
// dark rounded square, at every size an .iconset needs. Run it again only
// to change the icon, then commit the .icns it leaves:
//
//   swift latch/macos/make-icon.swift latch/macos/AppIcon.icns
import AppKit

let mark = "M64.2891 6.03791C103.801 -2.33144 140.954 15.1702 152.051 56.9783L152.059 56.9705C163.194 98.9657 146.53 157.832 93.5537 167.899C82.3869 170.025 67.326 171.372 53.1455 170.259V396.001L4 395.954V84.8739C4.57639 47.1219 27.183 13.9013 64.2891 6.03791ZM135.946 194.909V332.057C136.211 340.271 142.121 344.787 149.122 346.5L149.13 346.492C150.843 346.912 153.701 347.192 155.469 347.192H164.098V395.945H153.709C120.777 396.934 91.746 371.249 87.1982 338.986C87.1671 338.41 87.1278 337.827 87.0889 337.251C86.5126 328.827 86.3027 320.379 86.3027 311.939V194.909H135.946ZM396.003 328.515C396.003 344.125 391.035 355.422 387.889 361.23C367.751 398.383 319.82 406.581 286.584 381.441C261.104 401.224 224.177 400.905 200.13 379.386C190.162 370.463 177.688 354.425 177.688 332.93V194.909H226.459C226.459 231.447 226.451 293.231 226.459 323.867C226.49 329.325 226.747 334.557 230.049 339.142C237.026 348.936 251.137 349.637 258.714 339.944C262.833 333.949 261.868 329.255 262.062 320.037V194.909H311.612C311.612 228.504 311.597 298.183 311.612 324.568C311.628 327.223 311.714 329.909 312.212 332.377C314.463 344.6 330.216 349.723 339.95 341.759C344.038 338.8 347.169 333.84 347.185 329.114C347.27 287.188 347.192 41.4011 347.216 4.06819H396.003V328.515ZM197.996 22.5194C236.8 -9.91499 297.01 1.78684 321.571 45.6815C340.884 78.9335 332.536 123.257 303.389 148.069H303.381L303.271 148.163C269 178.48 214.395 174.851 185.785 138.711C157.51 103.902 163.055 50.7341 197.996 22.5194ZM83.1182 51.6688C68.011 51.5365 58.6272 60.6846 54.7959 74.5428C53.6434 78.6847 53.1611 82.9903 53.1533 87.2957V123.249C61.1118 124.759 69.7794 125.507 77.0137 124.674V124.681C95.9831 122.782 107.617 107.662 107.695 85.7225C107.765 66.4379 98.8794 51.8011 83.1182 51.6688ZM284.582 95.1746C291.972 70.7904 269.958 46.6475 245.335 49.7459L245.202 49.7615C217.55 53.4598 202.98 88.5723 222.16 110.053C242.142 132.895 276.125 123.49 284.582 95.1746Z"

/// The mark's outline: absolute M, C, L, H, V and Z, in a 400 by 400 box.
func outline(_ data: String) -> CGPath {
    let path = CGMutablePath()
    var numbers: [CGFloat] = []
    var command: Character = "M"
    var current = CGPoint.zero
    // Runs the command just read on the numbers that followed it; numbers
    // after a move's first pair are lines, as SVG says.
    func flush() {
        var n = numbers[...]
        func next() -> CGFloat { n.removeFirst() }
        if command == "Z" { path.closeSubpath() }
        while !n.isEmpty {
            switch command {
            case "M": current = CGPoint(x: next(), y: next()); path.move(to: current); command = "L"
            case "L": current = CGPoint(x: next(), y: next()); path.addLine(to: current)
            case "H": current.x = next(); path.addLine(to: current)
            case "V": current.y = next(); path.addLine(to: current)
            case "C":
                let c1 = CGPoint(x: next(), y: next()), c2 = CGPoint(x: next(), y: next())
                current = CGPoint(x: next(), y: next())
                path.addCurve(to: current, control1: c1, control2: c2)
            default: n = []
            }
        }
        numbers = []
    }
    var token = ""
    func number() { if !token.isEmpty { numbers.append(CGFloat(Double(token)!)); token = "" } }
    for char in data {
        if char.isLetter {
            number(); flush(); command = char
        } else if char == " " || char == "," {
            number()
        } else if char == "-" && !token.isEmpty {
            number(); token = "-"
        } else {
            token.append(char)
        }
    }
    number(); flush()
    return path
}

func png(size: Int) -> Data {
    let rep = NSBitmapImageRep(bitmapDataPlanes: nil, pixelsWide: size, pixelsHigh: size,
                               bitsPerSample: 8, samplesPerPixel: 4, hasAlpha: true, isPlanar: false,
                               colorSpaceName: .deviceRGB, bytesPerRow: 0, bitsPerPixel: 0)!
    let context = NSGraphicsContext(bitmapImageRep: rep)!.cgContext
    let scale = CGFloat(size) / 1024
    context.scaleBy(x: scale, y: scale)
    // macOS's icon grid: an 824-point body in the 1024 canvas.
    let body = CGRect(x: 100, y: 100, width: 824, height: 824)
    context.addPath(CGPath(roundedRect: body, cornerWidth: 185, cornerHeight: 185, transform: nil))
    context.setFillColor(CGColor(red: 0x11 / 255, green: 0x11 / 255, blue: 0x10 / 255, alpha: 1))
    context.fillPath()
    // The mark, 440 points across, centred; SVG's y runs down.
    var place = CGAffineTransform(translationX: 512 - 220, y: 512 + 220).scaledBy(x: 1.1, y: -1.1)
    context.addPath(outline(mark).copy(using: &place)!)
    context.setFillColor(CGColor(red: 0xD5 / 255, green: 0xEF / 255, blue: 0x8A / 255, alpha: 1))
    context.fillPath(using: .evenOdd)
    return rep.representation(using: .png, properties: [:])!
}

let output = URL(fileURLWithPath: CommandLine.arguments[1])
let iconset = FileManager.default.temporaryDirectory.appendingPathComponent("AppIcon.iconset")
try? FileManager.default.removeItem(at: iconset)
try FileManager.default.createDirectory(at: iconset, withIntermediateDirectories: true)
for points in [16, 32, 128, 256, 512] {
    try png(size: points).write(to: iconset.appendingPathComponent("icon_\(points)x\(points).png"))
    try png(size: points * 2).write(to: iconset.appendingPathComponent("icon_\(points)x\(points)@2x.png"))
}
let iconutil = Process()
iconutil.executableURL = URL(fileURLWithPath: "/usr/bin/iconutil")
iconutil.arguments = ["-c", "icns", iconset.path, "-o", output.path]
try iconutil.run()
iconutil.waitUntilExit()
exit(iconutil.terminationStatus)
