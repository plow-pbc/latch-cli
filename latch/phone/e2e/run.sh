#!/bin/sh
# The phone link, end to end, against a running host: for a login, an
# identity with secrets typed on the phone and a note, open a link with
# the owner's latch, save made-up values from WebKit as an iPhone
# (phone.mjs), check the host lists the items without a value, and remove
# them again. Not a gate: it needs a live host and the relay. Evidence goes
# to .dev/phone-e2e/.
# Usage: run.sh HOST, with LATCH the owner's latch (a device, not the host's
# own state).
set -eu
host=$1
latch=${LATCH:-latch}
port=${PORT:-8731}
cd "$(dirname "$0")"
root=$(cd ../../.. && pwd)
out="$root/.dev/phone-e2e/$(date +%Y%m%d-%H%M%S)"
mkdir -p "$out"
export PLAYWRIGHT_BROWSERS_PATH="$root/.dev/playwright"
npm ci --silent --no-audit --no-fund
npx playwright install webkit

python3 -m http.server "$port" --bind 127.0.0.1 --directory ../dist > "$out/page-server.log" 2>&1 &
server=$!
trap 'kill $server' EXIT
sleep 1

# Made-up values, fresh each run, with letters hex never has, so a match
# in the logs below cannot be a hash or a key.
run=$(date +%s)
title="phone e2e $run"
password="Qz-made-up-$run"
ssn="Kw-$run"

check() { # KIND VALUES.json LINK-ARGS…
    kind=$1 values=$2
    shift 2
    "$latch" vault link --host "$host" --kind "$kind" --title "$title" \
        --page "http://localhost:$port/" "$@" > "$out/$kind-link.json"
    node phone.mjs "$out/$kind-link.json" "$values" "$out/$kind" | tee "$out/$kind.txt"
}

printf '{"password": "%s"}' "$password" > "$out/login-values.json"
check login "$out/login-values.json" --site toscrape.com --field username=phone-e2e
printf '{"ssn": "%s", "passport number": "%s"}' "$ssn" "$password" > "$out/identity-values.json"
check identity "$out/identity-values.json" --field "first name=Ada" --ask ssn --ask "passport number"
printf '{"notes": "%s"}' "$password" > "$out/note-values.json"
check note "$out/note-values.json"
rm "$out"/*-values.json

"$latch" vault list --host "$host" > "$out/list.json"
python3 - "$out/list.json" "$title" > "$out/saved.txt" <<'PY'
import json, sys
items = [i for i in json.load(open(sys.argv[1]))["items"] if i["title"] == sys.argv[2]]
kinds = sorted(i["kind"] for i in items)
assert kinds == ["identity", "login", "note"], kinds
for item in items:
    print(item["item"], item["revision"], item["kind"], [f["label"] for f in item["fields"]])
PY
cat "$out/saved.txt"
while read -r item revision _; do
    "$latch" vault remove --host "$host" "$item" --revision "$revision" > /dev/null
done < "$out/saved.txt"

# Nothing this machine wrote holds a value.
if grep -rl --exclude='*.png' -e "$password" -e "$ssn" "$out"; then
    echo "a made-up value reached the files above" >&2
    exit 1
fi
echo "phone e2e passed; evidence in $out"
