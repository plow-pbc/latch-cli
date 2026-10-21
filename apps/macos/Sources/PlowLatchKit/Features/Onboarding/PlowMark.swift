import SwiftUI

/// The Plow lettermark, from the brand's SVG path (viewBox 0 0 400 400,
/// even-odd fill).
public struct PlowMark: Shape {
    /// How much of the outline to draw, 0–1; the whole mark when nil.
    public var drawn: Double?

    public init(drawn: Double? = nil) {
        self.drawn = drawn
    }

    public func path(in rect: CGRect) -> Path {
        let scale = min(rect.width, rect.height) / 400
        let transform = CGAffineTransform(translationX: rect.midX - 200 * scale, y: rect.midY - 200 * scale)
            .scaledBy(x: scale, y: scale)
        return (drawn.map(Self.drawn) ?? Self.mark).applying(transform)
    }

    static let mark = subpaths.reduce(into: Path()) { $0.addPath($1) }

    /// The mark's separate outlines, each with its length, for drawing on.
    static let subpaths = parse(svgPath)
    static let lengths = subpaths.map(length)
    static let totalLength = lengths.reduce(0, +)

    /// The outline drawn on to `progress` the way SVG dashes it: every
    /// outline starts at once and draws at the same speed, `progress` of the
    /// total length each, so short ones finish first.
    static func drawn(_ progress: Double) -> Path {
        var path = Path()
        for (subpath, length) in zip(subpaths, lengths) {
            path.addPath(subpath.trimmedPath(from: 0, to: min(1, progress * totalLength / length)))
        }
        return path
    }

    /// A path's length, from straight lines and finely split curves.
    static func length(_ path: Path) -> Double {
        var total = 0.0
        var current = CGPoint.zero
        var start = CGPoint.zero
        func line(to point: CGPoint) {
            total += hypot(point.x - current.x, point.y - current.y)
            current = point
        }
        path.forEach { element in
            switch element {
            case let .move(to):
                current = to
                start = to
            case let .line(to):
                line(to: to)
            case let .curve(to, c1, c2):
                let p0 = current
                for step in 1...64 {
                    let t = Double(step) / 64
                    let u = 1 - t
                    let a = u * u * u, b = 3 * u * u * t, c = 3 * u * t * t, d = t * t * t
                    line(to: CGPoint(x: a * p0.x + b * c1.x + c * c2.x + d * to.x,
                                     y: a * p0.y + b * c1.y + c * c2.y + d * to.y))
                }
            case let .quadCurve(to, control):
                let p0 = current
                for step in 1...64 {
                    let t = Double(step) / 64
                    let u = 1 - t
                    line(to: CGPoint(x: u * u * p0.x + 2 * u * t * control.x + t * t * to.x,
                                     y: u * u * p0.y + 2 * u * t * control.y + t * t * to.y))
                }
            case .closeSubpath:
                line(to: start)
            }
        }
        return total
    }

    /// The brand's path data. It uses only absolute M, L, H, V, C and Z.
    static let svgPath = "M64.2891 6.03791C103.801 -2.33144 140.954 15.1702 152.051 56.9783L152.059 56.9705C163.194 98.9657 146.53 157.832 93.5537 167.899C82.3869 170.025 67.326 171.372 53.1455 170.259V396.001L4 395.954V84.8739C4.57639 47.1219 27.183 13.9013 64.2891 6.03791ZM135.946 194.909V332.057C136.211 340.271 142.121 344.787 149.122 346.5L149.13 346.492C150.843 346.912 153.701 347.192 155.469 347.192H164.098V395.945H153.709C120.777 396.934 91.746 371.249 87.1982 338.986C87.1671 338.41 87.1278 337.827 87.0889 337.251C86.5126 328.827 86.3027 320.379 86.3027 311.939V194.909H135.946ZM396.003 328.515C396.003 344.125 391.035 355.422 387.889 361.23C367.751 398.383 319.82 406.581 286.584 381.441C261.104 401.224 224.177 400.905 200.13 379.386C190.162 370.463 177.688 354.425 177.688 332.93V194.909H226.459C226.459 231.447 226.451 293.231 226.459 323.867C226.49 329.325 226.747 334.557 230.049 339.142C237.026 348.936 251.137 349.637 258.714 339.944C262.833 333.949 261.868 329.255 262.062 320.037V194.909H311.612C311.612 228.504 311.597 298.183 311.612 324.568C311.628 327.223 311.714 329.909 312.212 332.377C314.463 344.6 330.216 349.723 339.95 341.759C344.038 338.8 347.169 333.84 347.185 329.114C347.27 287.188 347.192 41.4011 347.216 4.06819H396.003V328.515ZM197.996 22.5194C236.8 -9.91499 297.01 1.78684 321.571 45.6815C340.884 78.9335 332.536 123.257 303.389 148.069H303.381L303.271 148.163C269 178.48 214.395 174.851 185.785 138.711C157.51 103.902 163.055 50.7341 197.996 22.5194ZM83.1182 51.6688C68.011 51.5365 58.6272 60.6846 54.7959 74.5428C53.6434 78.6847 53.1611 82.9903 53.1533 87.2957V123.249C61.1118 124.759 69.7794 125.507 77.0137 124.674V124.681C95.9831 122.782 107.617 107.662 107.695 85.7225C107.765 66.4379 98.8794 51.8011 83.1182 51.6688ZM284.582 95.1746C291.972 70.7904 269.958 46.6475 245.335 49.7459L245.202 49.7615C217.55 53.4598 202.98 88.5723 222.16 110.053C242.142 132.895 276.125 123.49 284.582 95.1746Z"

    static func parse(_ data: String) -> [Path] {
        var paths: [Path] = []
        var path = Path()
        var numbers: [CGFloat] = []
        var command: Character = "M"
        var current = CGPoint.zero
        var start = CGPoint.zero

        func flush() {
            var n = numbers[...]
            func take() -> CGFloat { n.removeFirst() }
            while !n.isEmpty {
                switch command {
                case "M":
                    if !path.isEmpty { paths.append(path) }
                    path = Path()
                    current = CGPoint(x: take(), y: take())
                    start = current
                    path.move(to: current)
                    command = "L" // further pairs after M are line-tos
                case "L":
                    current = CGPoint(x: take(), y: take())
                    path.addLine(to: current)
                case "H":
                    current.x = take()
                    path.addLine(to: current)
                case "V":
                    current.y = take()
                    path.addLine(to: current)
                case "C":
                    let c1 = CGPoint(x: take(), y: take())
                    let c2 = CGPoint(x: take(), y: take())
                    current = CGPoint(x: take(), y: take())
                    path.addCurve(to: current, control1: c1, control2: c2)
                default:
                    return
                }
            }
        }

        var token = ""
        func endNumber() {
            if let value = Double(token) { numbers.append(CGFloat(value)) }
            token = ""
        }
        for character in data {
            switch character {
            case "M", "L", "H", "V", "C", "Z":
                endNumber()
                flush()
                numbers = []
                command = character
                if character == "Z" {
                    path.closeSubpath()
                    current = start
                }
            case "-":
                endNumber()
                token = "-"
            case " ", ",":
                endNumber()
            default:
                token.append(character)
            }
        }
        endNumber()
        flush()
        if !path.isEmpty { paths.append(path) }
        return paths
    }
}
