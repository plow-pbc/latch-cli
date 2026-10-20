#!/bin/sh
# The phone link, end to end, against a running host:
#
# 1. For a login, an identity with secrets typed on the phone and a note,
#    open a link with the owner's latch and save made-up values from WebKit
#    as an iPhone (phone.mjs); each link must refuse a second use.
# 2. The host lists the items without a value.
# 3. A browser session on the host signs in to a practice site with the
#    saved login (`latch browser login`, as an agent calls it), and the
#    page must show that it is signed in.
# 4. The items are removed, and no made-up value may appear in the host's
#    serve log, its state folder (audit log included; the vault is sealed),
#    the client's state folder or anything the client printed.
#
# Not a gate: it needs a live host and the relay. Evidence goes to
# .dev/phone-e2e/.
# Usage: run.sh HOST, with
#   LATCH         the owner's latch (a device, not the host's own state)
#   CLIENT_STATE  that latch's state folder
#   HOST_STATE    the host's state folder, and HOST_LOG its serve log, both
#                 readable from here (the host runs on this machine)
set -eu
host=$1
latch=${LATCH:-latch}
port=${PORT:-8731}
: "${CLIENT_STATE:?the owner client state folder}" "${HOST_STATE:?the host state folder}" "${HOST_LOG:?the host serve log}"
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
# in the files below cannot be a hash or a key.
run=$(date +%s)
title="phone e2e $run"
password="Qz-made-up-$run"
ssn="Kw-ssn-$run"
passport="Jx-passport-$run"
note="Vy-note-$run"
session="phone-e2e"

# Every client call's output goes to a file in $out, which step 4 searches.
client() { # NAME ARGS…
    name=$1
    shift
    "$latch" "$@" > "$out/$name.json" 2> "$out/$name.stderr"
}

check() { # KIND VALUES.json LINK-ARGS…
    kind=$1 values=$2
    shift 2
    client "$kind-link" vault link --host "$host" --kind "$kind" --title "$title" \
        --page "http://localhost:$port/" "$@"
    node phone.mjs "$out/$kind-link.json" "$values" "$out/$kind" > "$out/$kind.txt"
    cat "$out/$kind.txt"
}

# 1. The phone saves each item once.
printf '{"password": "%s"}' "$password" > "$out/login-values.json"
check login "$out/login-values.json" --site toscrape.com --field username=phone-e2e
printf '{"ssn": "%s", "passport number": "%s"}' "$ssn" "$passport" > "$out/identity-values.json"
check identity "$out/identity-values.json" --field "first name=Ada" --ask ssn --ask "passport number"
printf '{"notes": "%s"}' "$note" > "$out/note-values.json"
check note "$out/note-values.json"
rm "$out"/*-values.json

# 2. The host lists them, without values.
client list vault list --host "$host"
python3 - "$out/list.json" "$title" > "$out/saved.txt" <<'PY'
import json, sys
items = [i for i in json.load(open(sys.argv[1]))["items"] if i["title"] == sys.argv[2]]
kinds = sorted(i["kind"] for i in items)
assert kinds == ["identity", "login", "note"], kinds
for item in sorted(items, key=lambda i: i["kind"] != "login"):
    print(item["item"], item["revision"], item["kind"], [f["label"] for f in item["fields"]])
PY
cat "$out/saved.txt"
login_item=$(awk '$3=="login"{print $1}' "$out/saved.txt")

# 3. A browser session on the host signs in with the saved login.
client open browser --host "$host" --session "$session" open https://quotes.toscrape.com/login
client observe browser --host "$host" --session "$session" observe
client login browser --host "$host" --session "$session" login --item "$login_item"
client close browser --host "$host" --session "$session" close
python3 - "$out" "$login_item" <<'PY'
import json, sys
out, item = sys.argv[1], sys.argv[2]
observed = json.load(open(f"{out}/observe.json"))
assert any(v["item"] == item for v in observed["vault"]), observed["vault"]
login = json.load(open(f"{out}/login.json"))
assert login["submitted"] is True, login
assert "Logout" in login["lines"], login["lines"]
print(f"browser login with {item}: submitted, page shows Logout")
PY

# 4. Remove the items, then look for the values everywhere they must not be.
while read -r item revision _; do
    client "remove-$item" vault remove --host "$host" "$item" --revision "$revision"
done < "$out/saved.txt"
found=0
for value in "$password" "$ssn" "$passport" "$note"; do
    for place in "$HOST_LOG" "$HOST_STATE" "$CLIENT_STATE" "$out"; do
        if grep -rl --exclude='*.png' -e "$value" "$place"; then
            echo "a made-up value reached $place" >&2
            found=1
        fi
    done
done
[ "$found" = 0 ]
echo "no made-up value in: $HOST_LOG, $HOST_STATE, $CLIENT_STATE, $out (screenshots aside)"
echo "phone e2e passed; evidence in $out"
