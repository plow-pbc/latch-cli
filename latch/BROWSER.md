# Browsers on a latch host

A host runs Chrome for Testing (CfT), the release pinned in
`src/browser/cft.json`. latch downloads it, it does not ship it: on macOS
and on Linux arm64 (the Pi) `install.sh` fetches the zip for the machine's
architecture, checks its sha256 against the pin, unpacks it (`ditto` on
macOS, `unzip` on Linux) into `<state-dir>/plugins/cft/<version>/` (beside
older versions, as gog's are) and copies the pin in as `manifest.json`,
which marks the install complete. On Linux it also installs Xvfb, for
hosts with no screen.

A build may add launch arguments of its own (`args`) after the recipe's.
The macOS builds add `--disable-crashpad-for-testing`, which keeps Chrome's
crash handler from writing to the user's Library; the Linux arm64 build
leaves it out, because with it the network service crashes at once on the
Pi ("FD ownership violation") and no page loads.

Before every launch latch hashes the whole unpacked build (every file's
bytes and execute bit, every link and folder) and refuses one that differs
from the pin's `tree_sha256` (`engine_unverified`), so a changed framework
or helper is caught, not only a changed main binary. To pin a new release,
run `python3 scripts/cft-pin.py VERSION` on a Mac and put its output in
`builds`.

## Camoufox, the second engine

`install.sh --with-camoufox` (macOS) also installs Camoufox, pinned in
`src/browser/camoufox.json` with the runtime latch drives it through, into
`<state-dir>/plugins/camoufox/<version>/`, about 1 GB:

- `camoufox/`: the Camoufox build (zip sha256-checked, unpacked with
  `ditto`) and the `version.json` camoufox-js reads;
- `node/`: the pinned Node (tarball sha256-checked);
- `runtime/`: the helper's package from `camoufox/` (Playwright below 1.63
  and camoufox-js, exactly as `package-lock.json` pins them), installed
  with `npm ci --ignore-scripts`. camoufox-js's native SQLite addon is
  replaced by a few lines over Node's own (`camoufox/sqlite/`), so no
  package downloads anything unchecked.

Before every launch latch checks each part's tree hash against the pin.
`python3 scripts/camoufox-pin.py` prints them for every pinned Mac build.

The driver (`src/browser/camoufox.rs`) runs latch's helper,
`camoufox/helper.mjs` (built into latch, written into the session's
scratch folder), on that Node. The helper speaks the driver operations as
JSON lines over its stdio and drives Camoufox through Playwright, which
reaches it over its own pipe (`-juggler-pipe`), never a port. latch's page
reader runs in Camoufox's isolated world; the agent's `eval` in the page's
own world (Camoufox's `mw:`). Each profile gets one fingerprint, made on its
first launch with the host's OS and kept in the profile (`camoufox.json`,
0600), so a site sees the same device every time. Camoufox ships with
password saving and form autofill off; its uBlock add-on is left out.

A profile is bound to its engine when it is made. `open` makes a missing
profile for CfT; for Camoufox, make it first, on the host:

    latch browser profile create shop --engine camoufox
    latch browser open --profile shop https://example.com   # from a client

## Sessions and profiles

Each profile in use has one running browser, its **runtime**, driven by
one long-lived browser worker (`latch browser-worker`, hidden), which the
host talks to over stdio. The worker drives CfT over
`--remote-debugging-pipe` only: CDP on Chrome's fds 3 and 4, never a port;
Camoufox through its helper (above).

Each `open` makes a new **session** in its profile's runtime, starting the
runtime if none is running: a window of its own, and every tab or popup
opened from it, with its own snapshots, downloads and dialogs. Sessions on
one profile share its logins (cookies and site storage), never each
other's tabs. The worker makes one call at a time; sessions take turns call
by call, so one session's plan or wait never holds the browser for
the others (a wait checks its condition a few times a second, with the
browser free in between). A download belongs to the session whose page
started it, found from its frame; its file is staged under the download's
guid, so two sessions can download the same file name at once. One that no
session's page started is never listed.

A call has 60 seconds, every wait for the browser in it together (typing
adds 200 ms a character, a wait its own time): a page too busy to answer
gets `timeout`, never a call that hangs. A browser that leaves three
calls in a row unanswered is closed (`browser_gone` on the third; the next
`open` starts a new one), and a worker that has not answered 30 seconds
past a call's time is stopped (`browser_gone`).

Profiles live in `<state-dir>/browser/profiles/<name>/`: `profile.json`
binds the profile to its engine for life, `lock` is held by its runtime,
and `data/` is the engine's data folder (Chrome's, created with password
saving and autofill off, or Firefox's). A session closes after 30 minutes
without a call (counted from the end of its last one), and
closing one never touches the others; the runtime closes with its last
session, and the profile stays. `exec` and file workers are denied the
whole state directory, profiles included.

## Commands

```sh
latch browser --host my-mac open http://example.com/   # a new session: its id is in the answer
export LATCH_BROWSER_SESSION=sbkpanajj…                # or --session S on each command
latch browser --host my-mac observe                    # snapshot o2: refs e1, e2, … (on screen; --all for every one)
latch browser --host my-mac do 'type e3 "Acme" -> field: e3=Acme' 'click e7 -> path: /cart'
latch browser --host my-mac do --plan-file plan.json    # the canonical plan (- for stdin)
latch browser --host my-mac do --confirm c4f1a09b2e3d7  # carry out a confirmation, then any steps
latch browser --host my-mac do --history r3f09a1c2b7e4  # what a request did, step by step
latch browser --host my-mac do --history r3f09a1c2b7e4 --shot final --output stop.png
latch browser --host my-mac act click e7 --check "text: Added to cart"
latch browser --host my-mac act type e3 "Ada Lovelace"
latch browser --host my-mac act select e5 Blue
latch browser --host my-mac act press e3 Enter
latch browser --host my-mac act scroll down            # or: act scroll e9 up
latch browser --host my-mac act back
latch browser --host my-mac find --role button --name Download
latch browser --host my-mac wait --text "Saved" | --ref e4 --state hidden | --url-path /done
latch browser --host my-mac screenshot --output page.png
latch browser --host my-mac goto https://example.com/next   # the active tab loads it
latch browser --host my-mac text | tab new URL | tab switch t2 | tab close
latch browser --host my-mac text --raw > page.txt        # the same text alone, not JSON
latch browser --host my-mac dialog accept | dismiss
latch browser --host my-mac eval --script-file check.js   # its JSON value
latch browser --host my-mac upload e4 ./invoice.pdf
latch browser --host my-mac downloads                    # d1, d2, …
latch browser --host my-mac download get d1 --output report.csv
latch browser --host my-mac fill --item v1 --suggested s3f2a1c09bd   # a mapping observe suggested
latch browser --host my-mac fill --item v1 --field username=e4 --field password=e5 --press e6
latch browser --host my-mac fill --item v1 --field otp=e7 --press e8   # the current authenticator code
latch browser --host my-mac fill --item v2 --field number=e4 --field expiry=e5 --field cvv=e6 --press e7   # stops for a confirmation
latch browser --host my-mac handoff --reason captcha      # the owner takes over; waits until handed back
latch browser handoff done [HASH]                          # on the host: hand the session back
latch browser --host my-mac sessions | close
latch browser --host my-mac open --profile shop --task basket --reserve-site https://shop.example/
```

`text` answers the page's main-frame text, at most 20,000 characters
(`truncated` when cut); `text --raw` prints that same text alone, for a
pipe, with a note on stderr when it was cut. An error is the usual JSON on
stderr with its exit code either way.

`fill` takes values from the host's vault without the agent ever seeing
one, into the elements the agent maps or latch suggested; `observe` shows
the items that fit, what the page asks for and latch's suggestions.
[VAULT.md](VAULT.md) has the rules.

`open` answers the new session's id, once: 128 random bits, never derived
from the profile (`--profile` defaults to `default`). Every other session
command needs it, as `--session S` or `LATCH_BROWSER_SESSION`; there is no
default session, and a command without one gets `no_session` (exit 2),
which says so. Keep the id when `open` answers: latch never shows it again,
and a session whose id is lost closes itself after 30 idle minutes. The id is a capability, and a session is reachable only
with it and only by the caller (client key) that opened it: an id the
caller has no session for gets `no_such_session` (exit 1), another
caller's session `not_owner` (exit 4), and nothing reaches the session
either way. So two tasks under one client key each `open` their own
session, and neither can reach the other's without its id. `sessions` lists
the caller's sessions without their ids: task label (`open --task`),
profile, the site it was last on, its state and how long since its last
call. The audit records a short hash of an id, never the id, with the
task label, and the host's dashboard shows that hash on each row. An owner
device (a `device`-kind client of the host's account) ends any session by
it: `latch browser close --session <hash>`; for anyone else a hash is no
id.

Sessions on one profile share each site's account state (one cart, one
login). While another session on the profile acted on the same site
(registrable domain) in the last 15 minutes, an answer on that site
carries `shared_site`: that session's id hash and how many seconds ago.
`open --reserve-site URL` holds URL's site for the session until it
closes: another session's action there (a click, typing, `eval`, a fill,
a dialog) gets `site_busy` (exit 1) before anything is sent, and so does
one that would load a page of the site from elsewhere: opening it, `goto`
or `tab new` on it, Back to it, or a click, Enter or Space on a link or form that
leads there. Reading its page goes on. A page's own script can still
navigate where latch can't tell beforehand, and the reservation is this
host's only.

A vault fill holds the whole runtime: every session on the profile gets
`session_paused` until the fill's cleanup is confirmed, and while a value
a fill could not clear may be on a page, until that session is closed.

Each element in a snapshot has its ref, role and accessible name, its
value (never for a password or secret-looking field, which is marked
`secret`), checked/expanded/disabled/off-screen state, and the text of its
row or card. Every frame is read, cross-site and sandboxed ones included,
and listed with its effective origin (`self.origin` in latch's isolated
world; `opaque` for a sandboxed frame). A snapshot lists at most 200
elements, reads at most 50 frames, and leaves out a frame that does not
answer before the call's time is three quarters gone; any of these marks
it `truncated`. `observe --scope eN` lists one element's subtree.

An answer shows only the elements on screen, at most 60, and the frames
they are in, each frame's address without its query and cut to 80
characters; `omitted` says how many elements and frames it left out.
`observe --all` lists every element the snapshot read and every frame.
The snapshot holds them all either way, so a ref left out of the display
is still valid once it is shown, after `observe --all` or a scroll and an
`observe`. Only the display is cut: the audit, the gatekeeper and site
reservations read the full addresses.

A ref is bound to its frame's document: after a navigation, a ref from
before it is refused with `stale_snapshot`, with a fresh element list in
the error. A target that is hidden, disabled, covered or still moving is
refused with `covered_target`. Input is the engine's own mouse and keys at a
person's pace (`isTrusted` is true); `select` focuses the list and types
the option's label, as a person picks one without opening it. `type`
never types into a password or secret field (`secret_field`, exit 4).
While a dialog is open, every call but `dialog` is refused with
`dialog_open`.

`eval` runs the script's expression in the page's main frame, in the
page's own world (a promise is awaited), and answers its JSON value (at
most 1 MiB). Uploads and downloads move as files with matching sha256 on
both sides; a download is a handle (`d1`) until `download get`, which waits
a little for one still in progress and never hands over an incomplete one.
An engine without a command's optional feature (`eval`, `upload`,
`downloads`; CfT has all three) refuses it with `unsupported_capability`
before anything is dispatched.

The host audits each call: command, session, profile, engine, tab, origin
and path (never the query), snapshot, target ref and role, the outcome,
and the client's parent process id (or `--turn`, `LATCH_TURN`). A plan
(`do`, or `act`) records its request id, its
step count and how many are goals, its status and stop (step and code),
each step's kind and outcome, and per action the step and action number,
operation, ref and role, how it was chosen (`ref`, or `model` with the
operation's and target's `p`, and what was left out to fit), the risk
`p` (and for a model-chosen click or Enter, the pass's own risk answer,
`risk_pass_p`, and `risk_from`: `pass` or `own_call`), the confirmation id when one was used, the origin and path, the
dispatch (`sent`, `refused`, `unknown`) and its screenshot's size and
sha256. Each decision-model call records its purpose, provider, model,
the provider's request id, latency and the kinds of data sent, never
content. `scripts/plan-metrics.py STATE/audit.ndjson` reads these back:
per plan its actions, model calls, the model's total time and its cost
(an estimate: calls × Plow's stated $0.0001 a call), and over all plans
the plan share, confirmations and how they were answered, what followed
each stop, escapes to `eval`, turns, and stops by code.
Never page text, field values or typed text: `type` records the text's
length and sha256. `eval` records its script verbatim up to 4 KiB, its
size and sha256, and workaround tags (other frames, shadow roots,
synthetic events, setting field values, patching `fetch`/XHR, cookies or
storage, polling); files record name, size and sha256; an
`unsupported_capability` refusal records the capability, engine, origin
and command.

## The gatekeeper

An agent's browser commands pass the host's gatekeeper, which asks
whether the owner asked for them, in the owner's Plow chat with that
agent and the owner's rules on the host. The owner's own devices are
never judged. Judged, each before anything of it reaches the page:

- opening a site this session has not been judged for: `open URL`, or
  `goto URL` or `tab new URL` on another site;
- each plan (`do`): its steps as written with each step's values, the
  page it starts on (URL and title), the sites it names and the vault
  items it may fill (kind and title). Every `do` is judged, one of a
  single step too; only `act`'s one click, keypress or scroll is not;
- every vault fill: a `fill`, each fill a plan makes (a literal fill step
  or one a goal chose), and every `do --confirm` that releases a card or
  identity: the item's kind and title, the fields and the site, never a
  value. A refused fill in a plan stops the plan at that step, the reason
  in its `stop`;
- every `eval` (its script), upload and download (the file's name and
  size).

Clicks, typing, `act`, page reads, screenshots and the `--confirm` of an
ordinary click are not judged: the risk decision, confirmations and the
fill and pause checks above still hold. A refusal is exit 4,
`gatekeeper_denied`, and nothing was sent: with reason `not_covered`, ask
the owner in chat, saying exactly what you will do, then retry; with
`owner_said_no`, don't. A gatekeeper that can't judge (the chat or the
model unreachable) is exit 3, `gatekeeper_unavailable`, and nothing runs;
its message says what fixes it (below, under the decision model).
An agent whose line has more than one chat names the one it acts for
with `--chat C` or `LATCH_CHAT`. The call's audit record holds the
judgment as `gate` (verdict, probabilities, model, cost, latency, the
message ids read), never chat text. `just gated-shopping HOST_STATE OUT`
measures a shopping task's judgments on the decision model.

## Plans, literal steps and confirmations

The agent writes every step and every piece of text; latch's decision
model only chooses: whether a click or Enter can't be undone, whether a
sentence check holds, and, in a step written as a goal, the next
operation and its target among those the page offers.

**The plan command** (`do`) runs up to 10 steps in order, within
`--within S` (default 120, at most 300). In the short form each argument is
one step: `click|type|select|press eN ["text"]`, or
`fill --item vN (--field F=eN… | --suggested S) [--press eN]`; text to type goes in double
quotes (straight or curly). A step's check follows its last ` -> ` outside
quotes, so `type e3 "A -> B"` has none. These are the examples `do
--help` shows, one step per argument, each in single quotes so the shell
keeps its double quotes:

```sh
latch browser do 'type e3 "Ada Lovelace"' 'select e5 "Large"' 'press e3 Enter' 'click e7'   # literal steps on refs
latch browser do 'search for "blue mug"' 'add the note "Ring twice" for the driver'         # goals: they type only their quoted texts
latch browser do 'click e7 -> path: /cart' 'type e3 "Acme" -> field: e3=Acme'               # typed checks
latch browser do 'click e9 -> the cart shows 2 items'                                       # a sentence the model judges
latch browser do --fact street='12 Elm Road' 'enter the delivery street {street}'           # a fact a goal names
latch browser do --dry-run 'add the note "A -> B" for the driver'                           # the plan, nothing sent
```

A plan holds at most 10 steps. A ref is from the last page latch returned
(or `--snapshot`); once the page has changed, `observe` again for fresh
refs. Before going on after a stop, read `steps` (each step's outcome:
those before the stop ran) and `stop` (its step, code and message), and
send only what is left: resending the whole plan repeats what already ran.
An answer that exited 5 names its request: read `do --history R` first,
and never resend it. `--plan-file` takes the canonical
plan object, which every short form compiles to:

```json
{"steps": [{"literal": {"op": "type", "ref": "e3", "text": "Acme"},
            "check": {"kind": "field", "value": "e3=Acme"}},
           {"literal": {"op": "click", "ref": "e7"},
            "check": {"kind": "text", "value": "Saved", "require_change": true}}],
 "within": 60}
```

`act` is a plan of one literal step (`click`, `type`, `select`, `press`,
`scroll`, `back`) with an optional `--check`, and the same answer. Refs
are from any page latch returned to the session (`--snapshot`, else the
latest); each is pinned to its snapshot before anything runs.

**Instruction steps.** Any other step is a goal in words, such as
`add the Greek Salad to the cart -> text: Added`; its quoted texts, and
the plan's facts (`--fact NAME=TEXT`) it names as `{NAME}` outside quotes,
are the only texts it may type (a `{NAME}` the plan has no fact for is
refused before anything runs), and a quoted URL is the only place it may
`navigate` to. `--item vN` names a
vault item the plan may fill from: the model may then choose one of
latch's suggestions for it (item, mapping and button, or `none`), never a
field or a ref of its own; a plan that names none never fills. `--dry-run` prints the canonical plan
and sends nothing; `--output F` writes the page as the plan ended (or
stopped) to F on this machine. Each pass reads the page and asks the
decision model once for the step's status, the line that best shows it is
complete, the next operation and its target, offering only what the page
allows: elements not disabled or covered (a cookie modal hides what is
behind it), field × text pairs always with `none`, dropdown options with
the one already chosen marked. A typed check decides completion
(`checked`); otherwise the model's `complete` at p ≥ 0.7 with an evidence
line (`model_assessed`). Before its first action a step whose result is
already a field's value, the address or the title ends `already_held`;
page text alone, such as a message an earlier step left, never counts. A
pass stops, in this order, at a CAPTCHA (`needs_owner`), a code stage
or a passkey stage (`needs_fields`, before the model is asked; a passkey
stage goes on when a named item that fits holds a password, so the model
can take the page's password path), `blocked`,
`unconfirmed`, `stuck` or `already_true`, a budget (8 actions a step, 30 a
plan, 2 waits a step: `step_budget`, `plan_budget`, `deadline`), a target
below p 0.5 (`ambiguous_target`, with candidate refs), text that was not
supplied (`needs_text`, naming the field), a sign-in form whose login
the plan doesn't name (`needs_login`, with the items that fit), a fill
with no suggestion chosen (`needs_fields`, with the page's fields, offers
and other controls, for a literal `fill`), the same action on an
unchanged page, or, in a step without a typed check, a second click or
submit of the same control (`repeat`), or two actions that change nothing
(`no_progress`), and a click or Enter that can't be undone
(`confirm_needed`, as does a chosen fill that releases a card or identity
secret). A page a step's click opens in a new tab is followed: the plan
goes on there, nothing comes to the front. Lines written since the step began, even with the
same words as before (a message an add-to-cart writes again), are marked
as new for the model. With no decision model, an instruction step stops
`model_unavailable` before anything is sent.

**Checks**, decided by latch's code and waited for up to 10 s after the
step's action: `origin: https://shop.example` (the origin, whole),
`path: /checkout` (part of the path; never the host or query),
`query: q=salad`, `text: Saved` (the page text holds it),
`field: eN=<value|checked|unchecked>`, `download: complete` (a download
this step started), and `new text: …` (or `"require_change": true`: it must
not hold before the step, and must after). Anything else is a sentence the
decision model judges: met at yes ≥ 0.7 with an evidence line chosen while
"none" stays below 0.3. A sentence that names a task rather than a page
fact is refused before anything runs (`vague_condition`, exit 2).

**The answer** has `status` (`reached`: every step `checked`,
`model_assessed` or `already_held`; `dispatched`: every step ran but one had no check;
`stopped`, with `stop: {step, code, message, …}`), `review: true` when the
last step rests on the model alone, `steps` (per step: `outcome`, its
evidence, its action count), `trail` (per action: page URL and title,
operation, target with role, name and row text, `chosen` (`ref`, or
`model` with the operation's and target's p), the risk `p`,
the confirmation used, `result` `dispatched`/`refused`/`unknown`, what
changed, and `screenshot`: the handle `{"request": R, "shot": N}` of a
screenshot taken after it, or `{"suppressed": true}` after a vault fill or
while a filled value is on the page) and `page` (snapshot, URL, title, up to 40 lines, the element
list, bounded as every answer's is). A plan that stopped before sending any
action reads no new snapshot: its `page` is the tab's URL and title with a
`note` saying so, and a ref still names the last page latch returned (a
stop that offers refs, `candidates` or a `field`, reads one). A plan stops at the first step that does not pass, or at its
deadline; nothing is replayed or retried. Exit 0 is `reached` or
`dispatched`; a stop exits 1, or 4 for `confirm_needed`, `confirm_stale` and
`step_not_allowed`.

**Confirmations.** Before a click, latch asks the decision model whether it
would pay, place an order, send, publish, delete or close something, sign
out, create credentials or otherwise can't be undone, judged on the exact
element, its row text, the page title and the lines of the form or section
around it; an Enter in a field is judged as its form's submit (the form's
submit control, else the form), and so is `type` text with a line break,
which the engine types as Enter. For a click or Enter an instruction step
chose, the pass that chose it also answers the risk question of "the
click this pass would make" (or the Enter): below 0.3, for a click whose
target the pass was at least 0.9 sure of, that answer stands for the
decision and no call of its own is made, while the review before dispatch
still finds the whole context the pass read: the element (role, name, row)
or what the Enter submits, the lines around it and the page title;
otherwise (a button renamed, or the lines beside it changed, while the
model answered)
the element gets its own call, which alone can stop for a confirmation. The action is then made only if the
worker, at dispatch, still finds that element and those lines unchanged;
a page that changed while the model answered stops with `stale_snapshot`,
nothing sent. At p ≥ 0.5 the step stops with
`confirm_needed` (exit 4) before anything is sent, with a confirmation: its
id, the target's ref and snapshot, the reviewed context, and a screenshot
of the page as a handle (`{"request": R, "shot": "final"}`), which
`do --history R --shot final --output F` streams to the client with
matching sha256. A `fill` that
releases a card or identity secret field (card number, CVV, identity
numbers) always stops so, before any value is released, with the item's
title, the exact mapping, each destination frame's site and the press
button, whether it came standalone, as a plan step or as the model's
choice. The agent confirms
with `do --confirm C [STEP…]`, and only so: a click sent any other way,
`act click` on the same ref included, is judged again and stops with a new
confirmation if it still looks consequential. A confirmation is used once, in the same session and document, and
only while the element and the reviewed lines are unchanged; otherwise
`confirm_stale` with a fresh page (a total that changes from $10.00 to
$100.00 is refused, even with the same ref, URL and button name). It needs
no new risk decision, so it still works while the model is down. A
confirmed fill checks again at its press: if the button or the lines
around it changed once the values went in (a total that reprices as the
card number arrives), it does not press, clears the values, and answers
`confirm_stale`. A step
whose `new text` check already holds stops before the confirmation is
spent.

**No model, no pass.** With no model reachable, a click or Enter that needs
a risk decision, and a sentence check, stop with `model_unavailable` and
nothing sent. Literal `type`, `select`, `scroll` and `back`, typed checks
and confirmations already issued still work.

**Request ids and the journal.** The client makes an id per request, so it
knows it even if the answer is lost: an exit 5 names it. The host journals
each request in its state directory (`browser/journal/<session hash>/`,
0600): its steps, each action before it is sent and once its result is
known, each step's outcome and the end, each synced to disk. `do --history R`
reads it while the plan runs, after its answer was lost, and after the host
restarted: an action whose result was never recorded reads `unknown`, never
`not_sent`, and a request that never ended reads `interrupted`. An id is
never run twice (`duplicate_request`). Only the caller that sent a request
can read it, or fetch its screenshots with `do --history R --shot N
--output F`. A request's journal and screenshots are kept 7 days, after its
session closes too.

**Removed forms.** `act` on a name or description, `act --expect`,
`act --steps`, `act click --at X,Y`, `find "<description>"` and
`wait --for "<sentence>"` are usage errors: unknown arguments, or a
target that is not a ref, which points to the plan command.

**The decision model** runs in the host, never the worker, behind one
interface (`model.rs`): choose one option, or yes/no with a probability.
Answers are checked before use (an offered option; finite probabilities
covering the options and summing to 1). A choice below its top
probability gives way to the top option, and the call's audit record
keeps that answer's option ids and probabilities (`corrected`). Calls send
element names, row text, page lines, the title and non-secret field
values; never cookies, vault values, seeds or screenshots. The host holds
no model key: it asks Plow's decision route (`POST /v1/decisions`) with
its own Plow login, and Plow asks Jev (`typesafe/jev-1.13`) on OpenRouter
with zero data retention and bills the call to the host's Plow account, as
for any model call; the audit records provider `plow`, the model and Plow's
request id. Only a host set up for development calls TypeSafe jev
(`jev-1.13.0`) directly: `browser/model.json` in the state directory says
`{"provider": "jev"}` and its key is in `jev.key` (0600); a key file alone
is never used. A host not logged in to Plow, an account out of credits,
or a route that fails or can't be reached, means what needs a decision
stops with `model_unavailable`; there is no keyword stand-in. The message
says what fixes it: an account out of credits (Plow's 402) waits for the
host's account owner to add Plow credits, and every retry fails until
then; a login Plow refuses (401, 403) waits for the host's owner to run
`latch login` on the host again; an unreachable route or another error is
worth a retry later. The gatekeeper asks the same route, and its
`gatekeeper_unavailable` says the same. No command asks the model only to
report its health: a failure shows when something needs a decision.

## Handoff and screenless hosts

`handoff --reason captcha|2fa|passkey|signin [--within S]` hands the
session to the owner, for what only a person can do there. The call waits
until the owner hands it back (at most `--within`, 600 s by default).
Meanwhile that session alone is paused (`session_paused`; `close` still
works), the runtime's other sessions go on, and its window is the one
thing latch ever brings to the front. The host's dashboard shows the
pending handoff with the session's id hash, the reason and the start of
the caller's key (without a dashboard, the log says so). The owner hands
it back on the host with `latch browser handoff done`, naming the hash
when several are pending: it runs against the host's own state directory
(`browser/handoff/`), which file and exec workers can never reach. The
answer is `handed_back` with a fresh look at the page; every ref from
before the handoff is refused with `stale_snapshot`. At the deadline the
session is the agent's again (`timeout`), its old refs refused the same.

A host with no screen a person could use (any Linux host, or any host
started with `--headless-browsers`) answers `handoff` with `needs_owner`
and `"reason": "no_screen"` at once. On Linux, each headed profile
runtime's worker starts a private Xvfb display (`-displayfd`)
for its engine, up for as long as the runtime. It listens only on its
socket file in the runtime's private `/tmp`: no TCP port and no abstract
socket, which anything on the host's network namespace could reach. The
engine's `DISPLAY` is that file's path, not `:N`, which X clients look up
as an abstract socket first and could find another server's there. A
Linux host never gives the engine its own `DISPLAY`, even with a monitor
attached: its desktop is outside the runtime's bubblewrap.

## The engine is not inside latch's Seatbelt

macOS lets a Seatbelt-confined process re-apply only the profile it already
has, and every Chrome helper applies its own, so Chrome inside any latch
profile could only run with its sandbox off, which latch never does. The
host therefore starts the engine outside latch's Seatbelt, with Chrome's
own sandbox on for every renderer, GPU and utility process
(`--seatbelt-client`). What latch still controls:

- the pinned build, checked before each launch;
- a launch recipe that never opens a debug port, runs headless or turns
  the sandbox off (latch refuses one that does), with mock Keychain,
  no extensions and no crash reporter; headless comes only from the host's
  own setting (below);
- an environment of only `PATH`, `HOME`, `TMPDIR` and
  `BREAKPAD_DUMP_LOCATION`, the last three in the runtime's own scratch
  folder, removed when the runtime ends;
- every engine process in one process group, killed and waited for on
  close;
- where it goes. `open`, `goto`, `tab new` and a plan's navigation load
  only `http` and `https` addresses: `file:`, `chrome:`, `view-source:`,
  `data:` and every other scheme are refused with `url_refused` before
  anything is sent, and a tab a page takes off the web anyway is blanked,
  never read. Both engines reach the network only through a proxy in the
  worker that resolves each name itself and connects only to the address
  it checked, so the host's loopback, link-local and private addresses
  (Tailscale's included) are refused, by number or by a name that resolves
  to one. An owner who wants the browser on local services sets
  `{"allow_local_browsing": true}` in the state folder's `host.json`; it
  holds for every caller.

Engines run headed by default: on macOS the browser is a real window in the
owner's session, so the host must run in the logged-in desktop session.
`latch serve --headless-browsers` runs every session in Chrome's headless
mode instead, with no window: for a test host nobody watches. Sites block
headless browsers far more often, so a host people use stays headed.
They stay out of the owner's way: Chrome starts with no window, every tab
latch opens is a new window behind all others (its page still visible and
unthrottled), and switching tabs brings nothing to the front. A page's own
JavaScript dialog still raises its window, and so can a tab the page opens
(`target=_blank`, `window.open`); in Camoufox every window a page opens is
a tab that loads in the background. `handoff` brings the window forward on
purpose (Camoufox's app by its pid).
Camoufox runs the same way: outside latch's Seatbelt, with Firefox's own
sandbox on for every web content process; the helper and the browser each
in a process group of their own, both killed and waited for on close; an
environment of `PATH`, `HOME` and `TMPDIR` (the scratch folder), plus the
profile's fingerprint and `MOZ_CRASHREPORTER_DISABLE` for the browser and
the plugin's location for the helper.
A separate macOS user for engines is the later hardening.

## On Linux the engine runs inside bubblewrap

A Linux browser worker confines itself in bubblewrap before it starts
Xvfb or the engine, and Chrome's own sandbox runs inside that, in user,
PID and network namespaces of its own with its seccomp filters on (no
`--no-sandbox`, ever). The runtime sees:

- the system files the engine and display need, read-only (`/usr`, the
  loader cache, fonts, name resolution, certificates, the time zone and
  the CPU list), and no home folder;
- of the state directory, only its own profile (read-write) and the
  engine's plugin folder (read-only); everything else there is an empty,
  read-only folder;
- its scratch folder (read-write) and a private `/tmp`, which holds the
  display's socket;
- its own `/proc`, showing only its own processes;
- the outbound network, as the engine needs it; latch's seccomp filter
  still refuses mounts, `ptrace` and VM sockets.

It sees no areas. The worker's environment is still only `PATH`, and the
engine's the four variables above (and `DISPLAY`).

## Tests

`src/browser/tests.rs` runs real sessions against the pinned CfT
and Camoufox in `.dev/cft-state`, which `just cft` installs and `just test`
runs first;
CI does the same on its Macs. They run headless, so a test run never puts
a window on the screen. `just test-headed` (the `headed-tests` feature)
runs the same tests headed, where the checks that the engine never becomes
the front app have something to check; it is not part of the gate. `LATCH_CFT_STATE` points the test
at another state directory. Without an installed engine the test fails,
naming what to run. `src/browser/tests/steps.rs` covers literal steps,
checks, confirmations and the journal, with a fake
provider that answers from fixed facts about the test site and can be made
unreachable; the risk decision and a sentence check on jev itself are built
only with `--features live-model`, which `just test-model KEYFILE` runs.

On Linux they are built only with `--features linux-browser-tests` (the
gate's containers have no engine), against the CfT install.sh put in
`LATCH_CFT_STATE`. There the sandbox checks look at the user namespaces and
seccomp mode of the engine's processes, exec's view of the network at
`/proc/net`, and the front-app checks have no front app to watch.

These real-engine tests are also the driver conformance runner: each runs
once per engine, as `browser::tests::cft::NAME` and
`browser::tests::camoufox::NAME`, Camoufox on macOS only (and the same
under `fills` and `steps`).
`just conformance camoufox` runs one engine's checks; a new engine joins by
one line in the `conformance!` macro.
