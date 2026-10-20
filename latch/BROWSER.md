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
by call, so one session's step list or wait never holds the browser for
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
latch browser --host my-mac act click e7
latch browser --host my-mac act type e3 "Ada Lovelace"
latch browser --host my-mac act select e5 Blue
latch browser --host my-mac act press Enter            # or: act press e3 Enter
latch browser --host my-mac act scroll down            # or: act scroll e9 up
latch browser --host my-mac act back
latch browser --host my-mac act click --at 120,340     # CSS pixels in the page
latch browser --host my-mac act click "New expense" --expect-text "New expense"
latch browser --host my-mac act --steps steps.txt       # several steps, one per line (- for stdin)
latch browser --host my-mac act --history r3f09a1c2b7e4 # what a request did, step by step
latch browser --host my-mac find "the unpaid invoice's download"   # candidate refs, with p
latch browser --host my-mac find --role button --name Download
latch browser --host my-mac wait --text "Saved" | --ref e4 --state hidden | --for "the report is ready"
latch browser --host my-mac screenshot --output page.png
latch browser --host my-mac text | tab new URL | tab switch t2 | tab close
latch browser --host my-mac dialog accept | dismiss
latch browser --host my-mac eval --script-file check.js   # its JSON value
latch browser --host my-mac upload e4 ./invoice.pdf
latch browser --host my-mac downloads                    # d1, d2, …
latch browser --host my-mac download get d1 --output report.csv
latch browser --host my-mac login --item v1                # a vault login; latch finds the form
latch browser --host my-mac fill --item v2 --field number=e4 --field expiry=e5 --field cvv=e6 --press e7
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
task label.

Sessions on one profile share each site's account state (one cart, one
login). While another session on the profile acted on the same site
(registrable domain) in the last 15 minutes, an answer on that site
carries `shared_site`: that session's id hash and how many seconds ago.
`open --reserve-site URL` holds URL's site for the session until it
closes: another session's action there (a click, typing, `eval`, a fill,
a dialog, or loading a page of the site) gets `site_busy` (exit 1) before
anything is sent; reading its page goes on. The reservation is this
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
Each action answers `dispatched`, the target and how it was resolved,
what changed (tab, URL, title), the page's first lines, events (dialogs,
new tabs) and a fresh element list. While a dialog is open, every call
but `dialog` is refused with `dialog_open`.

`eval` runs the script's expression in the page's main frame, in the
page's own world (a promise is awaited), and answers its JSON value (at
most 1 MiB). Uploads and downloads move as files with matching sha256 on
both sides; a download is a handle (`d1`) until `download get`, which waits
a little for one still in progress and never hands over an incomplete one.
An engine without a command's optional feature (`eval`, `upload`,
`downloads`; CfT has all three) refuses it with `unsupported_capability`
before anything is dispatched.

The host audits each call: command, session, profile, engine, tab, origin
and path (never the query), snapshot, target ref and role, how it was
resolved (and `p`), the outcome, and the client's parent process id (or
`--turn`, `LATCH_TURN`). An `act` records its request id, step count,
actions dispatched, and per step the action, ref, role, how and `p`,
`dispatch`, expectation kinds and verdicts (never the expected text) and a
description's length (never its words); each decision-model call records
its purpose, provider, model, the provider's request id, latency and the
kinds of data sent, never content.
Never page text, field values or typed text: `type` records the text's
length and sha256. `eval` records its script verbatim up to 4 KiB, its
size and sha256, and workaround tags (other frames, shadow roots,
synthetic events, setting field values, patching `fetch`/XHR, cookies or
storage, polling); files record name, size and sha256; an
`unsupported_capability` refusal records the capability, engine, origin
and command.

## Several steps per turn

The agent writes every step and every piece of text, and says what each
step should lead to; latch's decision model only chooses and checks.

**Targets** resolve in order: a ref (bound to its snapshot); exactly one
element, among those that fit the action, whose accessible name is the
target (case and spacing aside, never "contains"); else one model choice
among them, each with its row text, plus "none", accepted at p ≥ 0.8.
Otherwise the step stops with `no_such_target` or `ambiguous_target` and up
to three candidates. Password and secret fields are never targets. The
result says `resolved_by`: `ref`, `name` or `model` (with `p`).

**Expectations**, on any step, wait up to `--within S` (default 10):
typed checks first, decided by latch (`--expect-url` prefix,
`--expect-text`, `--expect-field eN=<value|checked|unchecked>`,
`--expect-download complete`, which counts only a finished download), then
`--expect "<sentence>"`, met only at yes ≥ 0.9 with one quoted page line
as evidence (`met`, `not_met` or `unsure`, with `p`). A sentence that names
a task rather than something a page shows is refused before anything runs
(`vague_condition`, exit 2).

**Step lists** (`act --steps`) take one step per line in `act`'s own form,
quoted as a shell would; `#` starts a comment. The whole list, every
step's operands included, is checked before its first step (at most 10
steps). It stops at the first
unresolved target, refusal, unmet or unsure expectation, a step that led to
a sign-in form (`needs_login`) or a CAPTCHA (`needs_owner`), or a timeout;
later steps don't run, and nothing is replayed or retried. The answer has
`status`, `executed` and a `trace` (per step: target, how, `dispatch`
`dispatched`/`not_sent`/`unknown`, expectation verdicts, events), and ends
with the page's first lines and an element list, as every stop does. A
single `act` is a list of one, answered flat.

**Request ids.** The client makes one per `act` and sends it, so it knows
it even if the answer is lost: an exit 5 names it. The host keeps every
request's trace for the session's lifetime, updated as each step ends;
`act --history R` reads it, even while the list still runs: a step reads
`dispatched` as soon as its action is out, before its expectations are
waited for. An id is never run twice (`duplicate_request`).

**Risky clicks need a ref.** A click on a page whose element list has never
reached the agent (internal readings don't count) is reviewed first: if it
would pay, order, send, delete, create credentials or otherwise can't be
undone, the step stops with `confirm_target` (exit 4) and the target's ref
and snapshot, however it was named. The stop shows the page, so repeating
the click by ref goes through. The model judges this; without one, a fixed
word list over the name and row text does. `click --at` names no element
to review, so on a page the agent hasn't seen it is refused outright.

**The decision model** runs in the host, never the worker, behind one
interface (`model.rs`): choose one option, or yes/no with a probability.
Answers are checked before use (an offered option; finite probabilities
covering the options, summing to 1, peaking at the choice). Calls send
element names, row text, page lines, the title and non-secret field
values; never cookies, vault values, seeds or screenshots. For development
it is TypeSafe jev (`jev-1.13.0`), with its key in `jev.key` (0600) in the
host's state directory; no key is ever part of latch, and the product path
goes through Plow. With no key, or the model failing, descriptions resolve
by exact name only, sentences answer `assist_off`, and everything else
works.

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
  no extensions and no crash reporter;
- an environment of only `PATH`, `HOME`, `TMPDIR` and
  `BREAKPAD_DUMP_LOCATION`, the last three in the session's own scratch
  folder, removed when the session ends;
- every engine process in one process group, killed and waited for on
  close.

Engines always run headed: on macOS the browser is a real window in the
owner's session, so the host must run in the logged-in desktop session.
They stay out of the owner's way: Chrome starts with no window, every tab
latch opens is a new window behind all others (its page still visible and
unthrottled), and switching tabs brings nothing to the front. A page's own
JavaScript dialog still raises its window; `handoff` will bring it forward
on purpose. This is the pin's one `focus` switch (`background`; `front` brings
windows and tab switches forward instead).
A separate macOS user for engines is the later hardening.

## Tests

`src/browser/tests.rs` (macOS) runs a real, headed session against the
pinned CfT in `.dev/cft-state`, which `just cft` installs and `just test`
runs first; CI does the same on its Macs. `LATCH_CFT_STATE` points the test
at another state directory. Without an installed engine the test fails,
naming what to run. `src/browser/tests/steps.rs` covers step lists with
no model and with a fake provider that answers from fixed facts about the
test site; the same task on jev itself is built only with
`--features live-model`, which `just test-model KEYFILE` runs.
