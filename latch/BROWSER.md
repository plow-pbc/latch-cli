# Browsers on a latch host

A host runs Chrome for Testing (CfT), the release pinned in
`src/browser/cft.json`. latch downloads it, it does not ship it: on macOS
`install.sh` fetches the zip for the machine's architecture, checks its
sha256 against the pin, unpacks it with `ditto` into
`<state-dir>/plugins/cft/<version>/` (beside older versions, as gog's are)
and copies the pin in as `manifest.json`, which marks the install complete.

Before every launch latch hashes the whole unpacked build (every file's
bytes and execute bit, every link and folder) and refuses one that differs
from the pin's `tree_sha256` (`engine_unverified`), so a changed framework
or helper is caught, not only a changed main binary. To pin a new release,
run `python3 scripts/cft-pin.py VERSION` on a Mac and put its output in
`builds`.

## Sessions and profiles

Each profile in use has one running browser, its **runtime**, driven by
one long-lived browser worker (`latch browser-worker`, hidden), which the
host talks to over stdio. The worker drives CfT over
`--remote-debugging-pipe` only: CDP on Chrome's fds 3 and 4, never a port.

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

Profiles live in `<state-dir>/browser/profiles/<name>/`: `profile.json`
binds the profile to its engine for life, `lock` is held by its runtime,
and `data/` is Chrome's user data folder, created with password saving and
autofill off. A session closes after 30 minutes without a call, and
closing one never touches the others; the runtime closes with its last
session, and the profile stays. `exec` and file workers are denied the
whole state directory, profiles included.

## Commands

```sh
latch browser --host my-mac open http://example.com/   # a new session: its id is in the answer
export LATCH_BROWSER_SESSION=sbkpanajj…                # or --session S on each command
latch browser --host my-mac observe                    # snapshot o2: refs e1, e2, …
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
latch browser --host my-mac text | tab new URL | tab switch t2 | tab close
latch browser --host my-mac dialog accept | dismiss
latch browser --host my-mac eval --script-file check.js   # its JSON value
latch browser --host my-mac upload e4 ./invoice.pdf
latch browser --host my-mac downloads                    # d1, d2, …
latch browser --host my-mac download get d1 --output report.csv
latch browser --host my-mac login --item v1                # a vault login; latch finds the form
latch browser --host my-mac fill --item v2 --field number=e4 --field expiry=e5 --field cvv=e6 --press e7   # stops for a confirmation
latch browser --host my-mac fill-code --item v1            # the current authenticator code
latch browser --host my-mac sessions | close
latch browser --host my-mac open --profile shop --task basket --reserve-site https://shop.example/
```

`login`, `fill` and `fill-code` take values from the host's vault without
the agent ever seeing one; [VAULT.md](VAULT.md) has the rules.

`open` answers the new session's id, once: 128 random bits, never derived
from the profile (`--profile` defaults to `default`). Every other session
command needs it, as `--session S` or `LATCH_BROWSER_SESSION`; there is no
default session. The id is a capability, and a session is reachable only
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
one that would load a page of the site from elsewhere: opening it, `tab
new` on it, Back to it, or a click, Enter or Space on a link or form that
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
elements (`truncated`), or one element's subtree with `observe --scope eN`.

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
(`do`, or `act`) records its request id, whether it came by `act`, its
step count, the step policy, its status and stop (step and code), and per
action the step and action number, operation, ref and role, how it was
chosen (`ref`), the risk `p`, the confirmation id when one was used, the
origin and path, and the dispatch (`sent`, `refused`, `unknown`); a
`step_not_allowed` refusal records the step and policy. Each
decision-model call records its purpose, provider, model, the provider's
request id, latency and the kinds of data sent, never content.
Never page text, field values or typed text: `type` records the text's
length and sha256. `eval` records its script verbatim up to 4 KiB, its
size and sha256, and workaround tags (other frames, shadow roots,
synthetic events, setting field values, patching `fetch`/XHR, cookies or
storage, polling); files record name, size and sha256; an
`unsupported_capability` refusal records the capability, engine, origin
and command.

## Plans, literal steps and confirmations

The agent writes every step and every piece of text; latch's decision
model only chooses: whether a click or Enter can't be undone, and whether a
sentence check holds.

**The plan command** (`do`) runs up to 10 steps in order, within
`--within S` (default 120, at most 300). In the short form each argument is
one step: `click|type|select|press eN ["text"]`, or
`fill --item vN --field F=eN… [--press eN]`; text to type goes in double
quotes (straight or curly). A step's check follows its last ` -> ` outside
quotes, so `type e3 "A -> B"` has none. `--plan-file` takes the canonical
plan object, which every short form compiles to:

```json
{"steps": [{"literal": {"op": "type", "ref": "e3", "text": "Acme"},
            "check": {"kind": "field", "value": "e3=Acme"}},
           {"literal": {"op": "click", "ref": "e7"},
            "check": {"kind": "text", "value": "Saved", "require_change": true}}],
 "within": 60}
```

`act` is a plan of one literal step (`click`, `type`, `select`, `press`,
`scroll`, `back`) with an optional `--check`, and the same answer. Steps
written as a goal in words (instruction steps) are refused for now with
`step_not_allowed`. Refs are from any page latch returned to the session
(`--snapshot`, else the latest); each is pinned to its snapshot before
anything runs.

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

**The answer** has `status` (`reached`: every step `checked` or
`model_assessed`; `dispatched`: every step ran but one had no check;
`stopped`, with `stop: {step, code, message, …}`), `review: true` when the
last step rests on the model alone, `steps` (per step: `outcome`, its
evidence, its action count), `trail` (per action: page URL and title,
operation, target with role, name and row text, `chosen: ref`, the risk `p`,
the confirmation used, `result` `dispatched`/`refused`/`unknown`, what
changed) and `page` (snapshot, URL, title, up to 40 lines, the element
list). A plan stops at the first step that does not pass, at a sign-in form
(`needs_login`) or a CAPTCHA (`needs_owner`) a step led to, or at its
deadline; nothing is replayed or retried. Exit 0 is `reached` or
`dispatched`; a stop exits 1, or 4 for `confirm_needed`, `confirm_stale` and
`step_not_allowed`.

**Confirmations.** Before a click, latch asks the decision model whether it
would pay, place an order, send, publish, delete or close something, sign
out, create credentials or otherwise can't be undone, judged on the exact
element, its row text, the page title and the lines of the form or section
around it; an Enter in a field is judged as its form's submit (the form's
submit control, else the form), and so is `type` text with a line break,
which the engine types as Enter. The action is then made only if the
worker, at dispatch, still finds that element and those lines unchanged;
a page that changed while the model answered stops with `stale_snapshot`,
nothing sent. At p ≥ 0.5 the step stops with
`confirm_needed` (exit 4) before anything is sent, with a confirmation: its
id, the target's ref and snapshot, the reviewed context, and a screenshot
of the page as a handle (`{"request": R, "shot": "final"}`), which
`do --history R --shot final --output F` streams to the client with
matching sha256. A `fill` that
releases a secret field (card number, CVV, identity numbers) always stops
so, before any value is released, with the item's title and the `--press`
button, whether it came standalone or as a plan step. The agent confirms
with `do --confirm C [STEP…]`, or `act click eN` on that same ref (not for
fills). A confirmation is used once, in the same session and document, and
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

**The step policy** (a host setting, `browser/policy.json` in the state
directory: `{"literal_steps": "any" | "stop" | "none"}`, `any` without one)
decides which literal steps a session may run: refs from any page latch
returned, only refs from the page the session's latest stop returned, or
none. A forbidden step is refused with `step_not_allowed` (exit 4) before
anything is sent, whether it came by `act`, the short form or a plan file.
Confirmations are never forbidden.

**Request ids and the journal.** The client makes an id per request, so it
knows it even if the answer is lost: an exit 5 names it. The host journals
each request in its state directory (`browser/journal/<session hash>/`,
0600): its steps, each action before it is sent and once its result is
known, each step's outcome and the end, each synced to disk. `do --history R`
reads it while the plan runs, after its answer was lost, and after the host
restarted: an action whose result was never recorded reads `unknown`, never
`not_sent`, and a request that never ended reads `interrupted`. An id is
never run twice (`duplicate_request`). Only the caller that sent a request
can read it.

**Removed forms.** `act` on a name or description, `act --expect`,
`act --steps`, `act click --at X,Y`, `find "<description>"` and
`wait --for "<sentence>"` are usage errors (`removed_form`) that point to
the plan command.

**The decision model** runs in the host, never the worker, behind one
interface (`model.rs`): choose one option, or yes/no with a probability.
Answers are checked before use (an offered option; finite probabilities
covering the options, summing to 1, peaking at the choice). Calls send
element names, row text, page lines, the title and non-secret field
values; never cookies, vault values, seeds or screenshots. The host holds
no model key: it asks Plow's decision route (`POST /v1/latch/decide`) with
its own Plow login, and Plow asks Jev (`typesafe/jev-1.13`) on OpenRouter
with zero data retention; the audit records provider `plow`, the model and
Plow's request id. Only a host set up for development calls TypeSafe jev
(`jev-1.13.0`) directly: `browser/model.json` in the state directory says
`{"provider": "jev"}` and its key is in `jev.key` (0600); a key file alone
is never used. A host not logged in to Plow, or a route that fails, can't
be reached or is rate limited, means what needs a decision stops with
`model_unavailable`; there is no keyword stand-in.

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
  close.

Engines run headed by default: on macOS the browser is a real window in the
owner's session, so the host must run in the logged-in desktop session.
`latch serve --headless-browsers` runs every session in Chrome's headless
mode instead, with no window: for a test host nobody watches. Sites block
headless browsers far more often, so a host people use stays headed.
They stay out of the owner's way: Chrome starts with no window, every tab
latch opens is a new window behind all others (its page still visible and
unthrottled), and switching tabs brings nothing to the front. A page's own
JavaScript dialog still raises its window, and so can a tab the page opens
(`target=_blank`, `window.open`); `handoff` will bring it forward on
purpose. This is the pin's one `focus` switch (`background`; `front` brings
windows and tab switches forward instead).
A separate macOS user for engines is the later hardening.

## Tests

`src/browser/tests.rs` (macOS) runs real sessions against the pinned CfT
in `.dev/cft-state`, which `just cft` installs and `just test` runs first;
CI does the same on its Macs. They run headless, so a test run never puts
a window on the screen. `just test-headed` (the `headed-tests` feature)
runs the same tests headed, where the checks that the engine never becomes
the front app have something to check; it is not part of the gate. `LATCH_CFT_STATE` points the test
at another state directory. Without an installed engine the test fails,
naming what to run. `src/browser/tests/steps.rs` covers literal steps,
checks, confirmations, the step policy and the journal, with a fake
provider that answers from fixed facts about the test site and can be made
unreachable; the risk decision and a sentence check on jev itself are built
only with `--features live-model`, which `just test-model KEYFILE` runs.
