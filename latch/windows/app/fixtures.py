"""Copies the Mac app's snapshot fixtures (Snapshots.swift `Fixtures`) to
ui/fake/fixtures.json for the window's fake mode (ui/fake.js), so its stills
show the same made-up latch answers as the Mac's:

    python3 fixtures.py ../../gui/Sources/PlowLatchUI/App/Snapshots.swift ui/fake/fixtures.json
"""
import json, re, sys

src = open(sys.argv[1]).read()
body = src[src.index("nonisolated enum Fixtures {"):]
out = {}
# static let name = #"..."#   (one line)   or   #"""\n...\n"""#   (many)
for m in re.finditer(r'static let (\w+)\s*=\s*#"""\n(.*?)\n\s*"""#', body, re.S):
    out[m.group(1)] = re.sub(r'(?m)^ {8}', '', m.group(2))
for m in re.finditer(r'static let (\w+)\s*=\s*#"(.*?)"#\s*$', body, re.M):
    out.setdefault(m.group(1), m.group(2))
json.dump(out, open(sys.argv[2], "w"), indent=1)
print(len(out), "fixtures:", " ".join(sorted(out)))
