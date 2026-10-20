# latch browser proof of concept — design

Date: 2026-10-19 · Owner: head chef · Stage: **proof of concept on real hosts, on the path to the real product**, the same bar as `2026-10-14-latch-cli-poc-design.md`.
- It runs on owned test accounts and public test sites.
- It is not throwaway: the code must be clean enough for later work to build on directly.
- Reviewers don't demand the hardening that §2 defers.

Source of truth: `browser-api-proposal.html` (kitchen notes) and the chef's browser and vault rulings of 2026-10-19.

## 1. Goal

An agent drives a real browser on a latch host through `latch browser …`. Each call is one step, or one checked list of steps. The browser stays open on the host between calls. The POC shows, end to end:

- **The same agent commands on two engines,** Chrome for Testing (CfT) and Camoufox. Each plugs in as a driver behind one shared core; the plugin model is also the bake-off.
- **A numbered element list the agent acts on by ref**, plus page text and screenshots, on every host, a screenless Pi included.
- **Several steps per turn without a goal command.** A swappable decision model (jev in the POC) turns a description into an element, and judges a stated expectation by quoting the page line that proves it.
- **One vault per latch install, shared by every engine.**
  - A saved login is filled only on its exact site, in one checked step, with the agent paused.
  - No command returns a stored secret.
  - Authenticator codes are computed on the host.
  - Texted codes, CAPTCHAs and passkeys go to the owner through a handoff.
- **Everything goes through the host's gate and audit,** with no secret in any log, result or error.

## 2. Scope and non-goals

In scope: §1, on macOS hosts first and Linux hosts (the Pi) second. Not in the POC:

- **Raw browser commands** (raw CDP, Playwright scripts). `eval` stays, and latch logs what would have needed raw (§11).
- **The owner's remote view** of the host's browser. It is the direction for screenless hosts, but not this POC (§7, §8).
- **The owner's own Chrome or Safari** as a lane. It is a nice-to-have, a later bake-off lane.
- **Per-site consent or other privacy machinery** for the decision model. The model and its terms are picked later.
- **The gatekeeper** (script review, per-use approval of fills), **and Windows hosts.**
- **Also not:** CloakBrowser; network interception, extensions and closed shadow DOM; passkeys beyond handoff; a goal runner or `--until` repeats; other vault backends and secure-link UX; signed agent identity; stealth work beyond the pinned launch recipe.

## 3. Vocabulary

- **Engine / driver:** the browser program, and the code controlling one family of engines.
- **Profile:** a latch-owned browser data folder, bound to one engine for life.
- **Session:** one open browser on one profile. There is **one kind of session**: element actions, agent scripts (`eval`) and vault fills mix freely.
- **Snapshot / ref:** one reading of the page (`o3`) and an element in it (`e7`), valid only in that document.
- **Item:** a saved login in the vault.
- **Decision model:** a model that only chooses (one option, or yes/no with a probability), never writing text.

## 4. Architecture

```
agent machine                         latch host
┌──────────────────────────┐          ┌──────────────────────────────────────────────────┐
│ agent ─shell─▶ latch      │◀═ iroh ═▶│ latch serve: verify caller ▸ gate ▸ audit         │
│  browser … (client)       │          │   ├─ vault (one per install, backend interface)   │
└──────────────────────────┘          │   ├─ decision model client (key on host)          │
                                      │   └─▶ browser worker (sandboxed, one per session)  │
                                      │        shared core ─▶ engine driver ═pipe═▶ engine │
                                      └──────────────────────────────────────────────────┘
```

- **Client and transport are unchanged.** `latch browser` subcommands are ordinary client commands: `--host`, `--state-dir`, iroh, one stream per operation, the same exit codes. Screenshots and downloads stream to the client as files; uploads stream from it.
- **The host** gates every call, and holds the vault, the decision-model key and the audit log. It starts one **browser worker** per session and talks to it over the worker's stdio, one call at a time per session.
- **The browser worker** is long-lived for its session, unlike file and exec workers.
  - **macOS (chef, 2026-10-19):** macOS can't nest a different Seatbelt profile inside another, and every Chrome/Firefox helper applies its own (verified in B1's probe). So the host starts the engine **outside latch's Seatbelt**, with the engine's own sandbox on for every web-content process. What still holds: private pipe only, no port; profile in the state directory, hidden from exec and file workers; pinned binary checked by sha256; a minimal environment with no host secrets. What's lost: latch's confinement of the engine's main process. A separate macOS user for engines is the later hardening.
  - **Linux:** bubblewrap is applied before the engine starts, if the engine's sandbox nests inside it (B7 proves it). It sees its own profile folder (read-write), its engine's plugin folder (read-only), a per-session scratch folder and outbound network; nothing else of the state directory, and no areas.
  - File and exec workers never see profiles.
- **The shared core** runs in the worker and is engine-neutral. It owns tabs, the page reader (an isolated-world script that builds the element list), snapshots and refs, actions, waits, the login step and the pause. Secret filtering is written once, here.
- **Drivers** speak each engine's own channel over **private pipes only**, so no other latch worker can connect to a browser.
  - **CfT:** CDP over `--remote-debugging-pipe`, through a driver written in Rust and built into latch.
  - **Camoufox:** a small Node helper running Camoufox's pinned Playwright runtime, which speaks the driver operations as JSON lines over stdio.
- **Plugins.** Each engine installs to its own versioned plugin folder under the state directory, beside older versions, like gog. It holds the build and a manifest: engine, version, driver-contract version, binary and sha256 per OS and architecture, launch recipe, optional capabilities and licence notes. Pins are compiled into latch; the sha256 is checked before every launch.
- **Profiles** live in the state directory.
  - Only one session uses a profile at a time; `open` on a busy profile reuses its session.
  - Profiles are created with password saving and autofill off, and no extensions.
  - Sessions close after 30 minutes idle; the profile stays.

## 5. Agent commands

Every command below is `latch browser <cmd> --host H`; session commands also take `--session S`. Results are one JSON object on stdout with an `audit_id`; errors are `{"error":{"code","message",…}}` on stderr.

| Command | Does |
|---|---|
| `open [--profile P] [URL]` | Starts or reuses P's session (no `--profile`: profile `default`, CfT, created on first use). Returns session, engine, URL, title and a compact element list. |
| `observe [--tab T] [--scope eN]` | A snapshot of the element list, short page text, and the vault items that fit this page (titles and handles, never values). |
| `text` · `screenshot --output F` | Page text, or an image file on the client. |
| `act [--snapshot O] click\|type\|select\|press\|scroll\|back <target> [text]` | One action. The target is a ref, an exact name or a description (§6). `type` takes ordinary text, never a secret. `click --at X,Y` clicks a screen position. Moved or covered targets are refused. |
| `act --steps FILE\|-` | Several actions, one per line, in the same form (§6). |
| `find --role R --name N` · `find "<description>"` | Candidate refs (with p for descriptions). Never acts. |
| `wait --ref eN --state S` · `--text T` · `--for "<sentence>"` | Waits for a named condition, with a deadline. |
| `login --snapshot O --item I [--username eN] [--password eN] [--submit eN]` · `fill --snapshot O --item I --field F=eN… [--press eN]` · `fill-code --snapshot O --item I [--field eN] [--submit eN]` | Login, card/identity/note and TOTP steps (§7). |
| `handoff --reason captcha\|2fa\|passkey\|signin` | Pauses the agent until the owner says done (§7). |
| `upload eN FILE` · `downloads` · `download get D --output F` | Files move as files. |
| `dialog accept\|dismiss` · `tab new\|switch\|close` | Dialogs and tabs. |
| `eval --script-file F` | Agent JavaScript in the page, in any session. Refused while a filled secret is on the page. |
| `sessions` · `close` | List sessions, or end one (the profile stays). |

**Owner-device commands.** `latch vault add|list|remove|import` (§9) are client commands run from an **owner device**: a latch client state (its own `--state-dir`, logged in with `latch login` as kind `device`) distinct from the serving host's state. On the host's own machine the owner uses that separate client state, never the host's (the client already refuses to dial its own serving identity). The host accepts them only from a `device`-kind peer on its account; agent peers get exit 4.

**Owner-only, host-local commands.** These need the host's terminal and state directory, so `exec` can never reach them:
- `latch browser profile create|list|remove`;
- `latch browser handoff done`.

**Element list.** Each element carries its ref, role and accessible name; the value of ordinary fields (never password or secret-marked ones); checked, expanded, disabled and off-screen state; and **its row or card text**, which descriptions need. Off-screen elements are kept. The list is capped at 200 elements, with `truncated` set; `--scope` reads a subtree. Cross-site frames are included with their effective origin. A stale ref is refused, never re-guessed.

**Results.** An action reports `dispatched` (never "done"), the target and how it was resolved (`ref`, `name` or `model`, with p), what changed (URL, title, first lines on screen), any expectation verdict, and events (downloads, dialogs, new tabs). Every stop returns a compact element list, so the agent can steer without another `observe`.

| Exit | Meaning | Error codes |
|---|---|---|
| 0 | ok | — |
| 1 | failed | `stale_snapshot`, `covered_target`, `no_such_target`, `ambiguous_target`, `expect_not_met`, `expect_unsure`, `needs_login`, `needs_owner`, `session_paused`, `assist_off`, `unsupported_capability`, `timeout` |
| 2 | usage | bad arguments, `vague_condition`, unknown host |
| 3 | host unreachable; nothing sent | — |
| 4 | refused | `origin_mismatch`, `secret_on_page`, `confirm_target` |
| 5 | outcome unknown | read the request's history (`act --history R`) before retrying |

## 6. Several steps per turn, and the decision model

**No goal command.** The agent writes the steps and all text, and states what each step should lead to. The decision model only chooses and checks. It never picks the next action and never writes text.

**Targets** resolve in this order:
1. a ref (bound to its snapshot);
2. exactly one element with that accessible name (no model call);
3. one model choice among the elements that fit the action, with their row text, plus "none".

The model's choice is accepted only at p ≥ 0.8 (a starting value). Otherwise the step stops with `ambiguous_target` or `no_such_target` and up to three candidate refs with p. latch never uses "label contains" matching and never targets a password field.

**Expectations**, on any action:
- **Typed checks come first,** decided by latch's code: `--expect-url`, `--expect-text`, `--expect-field eN=<value|checked>`, `--expect-download complete`.
- **A sentence**, `--expect "…"`, counts as met only at yes ≥ 0.9 **and** with one quoted line of the page as evidence. It is reported as an assessment, with p and the line.
- A sentence naming a task rather than a page fact is refused before anything runs (`vague_condition`).
- Expectations wait up to `--within S` (default 10 s).
- A download counts only when complete.

**Step lists:**
- The whole list is checked before the first step runs.
- Each target is resolved on the page as it is at its step.
- One request id, generated by the client and sent with the request, so the client knows it even when the reply is lost. The host keeps each request's per-step outcomes (not sent, dispatched, expectation verdict) for the session's lifetime. `act --history R` returns that trace; it is how an exit 5 is resolved. Single `act` calls get the same.
- Trial cap: 10 steps.
- The list stops at the first unresolved target, unmet or unsure expectation, refusal, login wall or CAPTCHA (`needs_login`, `needs_owner`), or timeout. Later steps don't run.
- latch never replays an uncertain action and never retries a click by itself.

**Risky clicks need a ref.** A click that would pay, send, delete, place an order, create credentials or otherwise can't be undone, on a page the agent hasn't seen (no snapshot of that document has reached it), stops with `confirm_target` and the ref, however the target was named. The agent repeats the click by ref.
- The model's yes/no flags these clicks. Without a model, a fixed word list over the name and row text does.
- This is a target review, not an approval system.

**The decision model is swappable.**
- One host-side interface: choose one option from a list, or answer yes/no with a probability.
- Answers are validated before use: the choice must be an offered option; probabilities must be finite, sum to 1, and peak at the choice.
- POC implementation: TypeSafe jev, pinned to `jev-1.13.0`, with its key in a 0600 file in the host's state directory. **Dev-only:** no decision-model key is ever baked into latch or shipped to hosts (chef, 2026-10-19). The product path routes decision-model calls through Plow inference, authenticated by the host's Plow credential; the provider interface must make that a drop-in (chunk B5b).
- Calls run in the host process, never in the worker. They send element names, row text, page lines and non-secret field values. They never send cookies, vault values, seeds or screenshots.
- With no key, or the model down:
  - descriptions resolve by exact name only;
  - sentence expectations return `assist_off`;
  - everything else works.

**Metrics (the 1:13 lesson).**
- **Actions per request:** actions dispatched ÷ `latch browser` requests. latch measures this by itself.
- **Actions per agent turn:** actions dispatched ÷ agent turns. A turn can issue several commands, so this needs a turn id: the caller passes `--turn T` (or `LATCH_TURN`), and the audit also records the client's parent process id as a fallback grouping.
- **Target:** on the same tasks, at least 30% fewer agent turns than a refs-only baseline, with success no lower, and a median of 2.5 or more actions per turn on multi-page tasks.
- **Alarm:** a median below 1.5 means agents are bypassing it.
- **Also tracked:** escapes to `eval` or to one action per call; at least 99% precision on executed model choices; no observed false completions.

## 7. Login, codes and handoff

**`login --snapshot O --item I`:**
1. **Gate.** No per-profile binding (chef, 2026-10-19): any session may use any vault item, subject to its site rule; who may use what is the later gatekeeper's job. The host notes whether agent code ran in this session before the fill.
2. **Pause.** The whole browser is frozen for the agent: every tab, page text and screenshots. Calls return `session_paused`.
3. **Prepare.** latch picks the fields from snapshot O, unless the agent named them with `--username`, `--password` and `--submit`:
   - the one password field in a form; the text or email field before it in the same form; that form's submit button;
   - an identifier-first login (a page with only the username field): latch fills it, presses its continue button, waits up to 15 s for the password step (same or another site), re-reads that page itself, re-runs the origin match on it, then fills and submits — all inside the one paused `login` (chef, 2026-10-19, from the DoorDash test). A second step that asks for something other than a password (code, passkey) ends in `timeout` after cleanup;
   - the submit or continue button is picked only when exactly one candidate fits (the form's own submit button, else one plainly named continue/next/sign in/log in button for that field, never a button naming another provider); otherwise `ambiguous_target` with the candidates, and the agent names it;
   - no candidate, or more than one, stops with `ambiguous_target` and the candidate refs, and the agent names them.

   The driver resolves those refs and reports:
   - the field frame's **effective origin**, read as `self.origin` in latch's isolated world and bound to the frame and document (a frame URL or CDP `securityOrigin` misreports sandboxed frames);
   - the **top-level origin**;
   - the document id.
4. **Match (chef, 2026-10-19: subdomains match, for parity with the Latch app).** Both the field frame's and the top-level origin must be HTTPS and share the item's **registrable domain** (public-suffix list via `psl`, private suffixes included), unless the item allows other top-level sites. Plain `http`, an IP or local host, a missing page address, and opaque or uncertain origins are refused (`origin_mismatch`) — closing the old app's gaps. Only then does the vault release a value.
5. **Set.** One value at a time crosses the worker's private pipe. The driver re-checks element, document and origin and sets the value in one synchronous step, with no typing. A site that rejects it gets `needs_owner`, never a typing fallback.
6. **Submit and clean up.** latch clicks the item's submit element. It waits until the secret-bearing document is replaced, or the values are verified cleared (a URL change isn't enough). On failure it clears the fields. If cleanup can't be confirmed, the session stays paused.
7. **Resume.** The result is `submitted`, not "logged in". `eval` is refused while a filled secret is still on the page.

**`fill --snapshot O --item I --field <item-field>=eN … [--press eN]`** fills cards, identities and notes, as the Latch app did: the agent names which element gets which item field (`number`, `expiry`, `cvv`, `name`, address and identity fields). latch reshapes dates and expiries to the field's format and splits values across split boxes. The same pause and one-step set as `login` apply, with the per-kind origin rule from §9 ("Where each kind may fill"); no automatic field detection in the POC. Secret fields (card number, CVV, identity numbers, per the ported classifier) must leave the page before the agent resumes, so a fill that includes one **requires `--press eN`**: latch presses that button (a ref from a snapshot the agent has seen — the risky-click rule) and cleans up as in step 6 before resuming. Non-secret fields (name, address) may fill without `--press`. Site lists and auditing are in §9.

**`fill-code --snapshot O --item I`.** The TOTP seed stays in the vault. The host computes the code. The field and submit button are picked like `login`'s (one code field and its form's submit button, or `--field` / `--submit`). The same steps then run: pause, origin match, one-step write, submit, cleanup until the code's document is replaced or the field is verified cleared, resume with `submitted`.

**`handoff`.**
- The agent is paused, and the owner acts in the real browser window on the host.
- The host's dashboard shows the pending handoff, and the owner ends it with `latch browser handoff done`. Default timeout: 600 s.
- Refs from before the handoff are invalid afterwards.
- **On a host with no screen,** `handoff` returns `needs_owner` with `"reason":"no_screen"` at once. So do login walls that need a person. The agent tells its user that the site needs a host with a screen, such as the Mac. Sign-in without hands still works wherever a saved login (plus a TOTP code) is enough.

## 8. Screenshots and displays

- Screenshots to the agent are required on every host. They come from the engine, with the screen-to-page coordinate mapping, and are refused while a session is paused.
- **Engines always run headed, never in headless mode:** headless got blocked on far more sites in the browser-poc site check (9/26 against 24/26 headed).
- On macOS the browser is a real window in the owner's session. It is visible but **never steals focus** (chef, 2026-10-19): no startup window, every window and tab opened in the background, no tab activation; only `handoff` brings it to the front. Anti-throttling switches keep background windows rendering normally.
- On a screenless Linux host, each session's worker starts a private Xvfb display. A Pi with a monitor uses its desktop.

## 9. The vault

- **One vault per latch install,** shared by every engine and profile, on the host, behind a **backend interface**. A backend can:
  - list item metadata;
  - add, update and remove items;
  - release exactly one value, into the §7 fill path only;
  - call audit hooks.
- **Always latch's job, whatever the backend:** the origin rules, the login step, deciding a release, and value-free audit. A release needs only a matching origin; items are not bound to browser profiles (chef, 2026-10-19), and access rules belong to the later gatekeeper. There are no owner approvals for now (chef, 2026-10-19): a generic approval flow comes later, plugging into the gate; until then confirmations go back to the agent. Start with test and low-stakes accounts.
- **POC backend: latch's own store,** one file in the host's state directory, written only by `latch serve`, atomically.
  - Items are encrypted (AES-256-GCM, RustCrypto `aes-gcm`) with a random **data key**; the data key is locked by a **custody key**. In the POC the custody key is a 0600 file in the state directory on every OS (not the login Keychain: latch's `exec` sandbox can read login-Keychain items). The signed-app chunk (B4b) adds a Keychain half on macOS.
  - `exec` and file workers can read neither file; a conformance test checks it from inside both. On Linux the browser worker can't either (bubblewrap). **On macOS the browser engine runs outside latch's Seatbelt as the same user (§4), so a compromised engine main process could read both files: accepted risk with the chef's macOS ruling; a separate macOS user or B4b's Keychain half narrows it later.**
  - KeePass, Bitwarden, 1Password and Keychain items come later, behind the same interface.
- **Where each kind may fill** (all require HTTPS, non-opaque origins for both the field frame and the top-level page, and a page address; `http`, IP and local hosts are refused):
  - **login:** both origins share the item's registrable domain (§7 step 4); a login item must have a site.
  - **card, identity:** without a site list, any top-level site (parity with the Latch app); with an optional "only on these sites" list, the top-level page must share one of the listed registrable domains. The field frame may be another site (payment-processor iframes) but is checked and audited.
  - **note:** any top-level site, filled only into non-secret-marked fields named by the agent.
  - Every card and identity fill's top-level and field-frame sites are audited.
- **Item kinds, for parity with the Latch app:** login (username, password, optional TOTP seed — marked, since it holds both factors), card, identity, secure note. Field names follow rbw's Bitwarden-style model. Every item has a title, its site (required for logins; an optional site list for cards and identities) and a **revision** (an edit against an old revision is refused). Which fields are secret follows the Latch app's classifier, ported with its truth table as tests. TOTP via `totp-rs`, checked against the RFC 6238 vectors.
- **Commands** (owner-only): `latch vault add --kind login|card|identity|note …` (secrets prompted without echo, never from argv or the environment), `list` (metadata only), `remove`, and `import` (1Password 1PUX/CSV, Apple Passwords CSV, Bitwarden JSON/CSV, Chrome CSV; a picker, nothing selected by default; logins first, other kinds where the export carries them), porting the Latch app's import rules and tests. They go through `latch serve` over latch's own connection as an authenticated owner device, on the same machine too — no direct file edits. The secure link is out of v0; the design keeps it possible.
- **Reuse rule:** the Latch app's rules, formats and tests are ported to Rust; no Latch code is imported and no sidecar is added. Libraries: `url`, `psl`, `aes-gcm`, `totp-rs`, `csv`, `zip`.
- **The agent never sees a value.** No command returns a stored secret.
- **No browser password stores or autofill.** The agent never uses them. Signing in by hand through a handoff, or an already logged-in profile, is fine.
- **Accepted risk (chef's ruling):** an agent script that ran on the page before a fill can capture the value. It is the same class as the old Latch app, and the gatekeeper deals with it later. Every fill's audit line flags it.

## 10. Engines and the driver contract

| Operation | The driver must |
|---|---|
| launch · close | Start the pinned build with its launch recipe over a private pipe, on its profile, with no listening port. Lock the profile. Reap every child. |
| tabs and events | Report tabs, openers, navigations, replaced documents, downloads and dialogs. |
| frames | Report every frame, cross-site included, with a document id and its effective origin, or "opaque". |
| run latch's script | Run the shared page reader in an isolated world; run agent `eval` in the main world when latch passes it on. |
| resolve | Turn a ref into a live element in its own document. Report visible, enabled and not covered. Map coordinates through frames. |
| input | Browser-generated ("trusted"), human-paced mouse, keys and text. Never `element.click()` from script. |
| wait · screenshot | Named conditions with deadlines; pixels plus the coordinate mapping. |
| files · downloads · dialogs | Set upload files, report downloads as handles, answer dialogs. |
| prepare fill · checked fill | Report where a fill would go; re-check element, document and origin and write in one step, or refuse. Never retry. |
| pause · resume | Freeze all agent access to the browser. |
| capabilities | Report optional features. Anything missing returns `unsupported_capability` before dispatch, never a substitute. |

**Engines:**
- **CfT first:** Rust CDP driver; pinned CfT per OS and architecture; no `Runtime.enable`; `--disable-blink-features=AutomationControlled` in the recipe.
- **Camoufox second:** the Node helper, with pinned Node, Playwright (below 1.63) and Camoufox, `os` pinned to the host OS. An opt-in install (about 2.5 GB). The bar for dropping it is very high.

**Conformance suite.** latch accepts a driver only after it passes local test pages for: frames of every kind with the right origins; isolated globals with DOM changes visible; trusted input; checked fill refusing a wrong site, an opaque frame and a replaced document; the pause holding (screenshots included); dialogs, pop-ups and files; and, from inside `exec`, no reachable port and an unreadable vault key. An engine that fails only the fill tests is accepted without `login`, `fill` and `fill-code`.

## 11. Audit

The host's hash-chained NDJSON audit gets one record per browser operation:
- caller and agent;
- session, profile and engine;
- operation;
- target, how it was resolved, and p;
- origin and path (no query);
- expectation kind and verdict;
- outcome;
- request id, step, and the parent process id or turn id.

**Never written:** page text, field values, typed text (`type` records length and sha256), screenshots, vault values, codes or seeds.

Additions:
- **Fills:** item, both origins, outcome, and `agent_code_before_fill`.
- **`eval`:** the script verbatim, capped at 4 KiB, as with `exec` argv. Plus **workaround tags**, recorded only, when the script reaches into other frames or shadow roots, fires synthetic events or sets field values directly, patches `fetch`/`XMLHttpRequest`, reads cookies or storage, or polls in place of `wait`.
- **Every `unsupported_capability` hit:** capability, engine, origin and command. Together with the eval tags, this shows what raw commands would have been needed for.
- **Decision-model calls:** provider, model version, request id, latency, and the kinds of data sent, never the content.

## 12. Platforms, install and build

- **macOS hosts first; Linux hosts (the Pi) second,** on the bubblewrap sandbox latch hosts already use.
- `install.sh` installs:
  - the pinned CfT, sha256-checked, beside older versions;
  - on Linux, Xvfb from the system package manager;
  - Camoufox and its Node runtime only with `--with-camoufox`.
- The Pi uses CfT's `linux-arm64` build (per the platform metadata checked on 2026-10-19).
- The Rust driver and core live in the latch workspace on the existing CI. The Camoufox helper is a small Node package beside it, with a lockfile.
- A `latch browser` agent guide ships in the repo, and `latch browser --help` points to it. Its examples lead with step lists and expectations.

## 13. Testing (chef, 2026-10-19)

latch is primarily a consumer agent's browser, so testing answers two questions: can an agent log in and get things done, and which consumer sites block which engine. Source research: kitchen notes `research-browser-benchmarks.md`. No heavy Docker suites (WebArena and similar) for now.

- **Basics (adopted fixtures, served locally):** a handful of MiniWoB++ tasks (MIT) and Stagehand's interaction fixtures (MIT) — e.g. `choose-list`, `iframe_form_filling`, `multi_tab` — scored by their own pass checks, run through `latch browser`.
- **Real public sites, read-only:** a few WebVoyager tasks (Apache-2.0) such as arXiv help, GitHub latest release, a recipe with constraints; answers checked against the page at run time.
- **Security, where simple:** local pages adapted from WASP/AgentDojo (an injected post trying to leak a made-up canary to another local origin), SCAM (a lookalike login: no vault release, nothing reaches the "attacker" origin) and ST-WebAgentBench ("draft, don't send": zero sends).
- **latch's own promises:** a vault login with a handoff; the pause holding through a fill and its cleanup.
- **Consumer-site matrix (the bake-off's core):** for each engine (CfT; Camoufox once B8 lands), each site: does the home page load, can the agent search/browse logged out, does the login page load, and is the agent challenged or blocked (and where). Starting list: DoorDash, Costco, Redfin, plus a dozen common consumer sites (shopping, food delivery, travel, real estate, banking login pages). Logged-out only, except sites where the chef has added his own login to the vault and asked for it. Result: a table of site × engine → works / challenged / blocked, with screenshots, repeated on at least two days. This measures blocking; it does not build evasion.

## 14. Open questions

1. **Nested sandboxes.** macOS: answered — they can't nest; engines run outside latch's Seatbelt with their own sandbox on (§4, chef 2026-10-19). Linux: B7 proves bubblewrap nesting. Chrome's sandbox is never disabled; if Linux can't nest, stop and go back to the chef.
2. **Real login forms.** Does the one-step value set work on framework-built ones? Public practice forms are tested before "no typing" is frozen.
3. **CfT on the Pi.** If pinned `linux-arm64` CfT fails there, the fallback is Debian Chromium on the same driver, as a separate engine package.
4. **Approvals.** Answered for now: none; a generic approval flow comes later (not Plow's payment-approval flow), and confirmations go back to the agent.
5. **Which decision model, and its privacy terms,** before real accounts. **And how hosts reach it without a key: via Plow inference (chef's lean, 2026-10-19), which needs a Plow API route — B5b.**
6. **Calibration.** The thresholds (0.8, 0.9), the 10-step cap and the idle timeout get calibrated during the POC.
7. **Block rates.** The headed engines' block rates come from one IP in one run. Re-measure with complete drivers.
8. **What the logs show.** Do the `eval` workaround tags and `unsupported_capability` hits justify raw commands, or new ordinary commands?

## Chunks

### Global constraints

- **Product stage:** proof of concept on real hosts, not throwaway (header of this spec). Reviewers size the bar to that: clean, testable code that later work builds on; no hardening §2 defers.
- **Builds on latch-cli as it is** on branch `latch-cli` (spec `2026-10-14-latch-cli-poc-design.md`): same client/host split, iroh transport, gate, hash-chained audit, exit codes, state directory, plugin layout and `install.sh`. Nothing in §2's non-goals gets built.
- **One kind of session; no raw commands; no browser password stores or autofill; no headless mode; private pipes only — never a listening debug port.** These hold in every chunk.
- **No secret** (vault value, TOTP seed or code, decision-model key) ever appears in a result, error, log line, audit record, test output or commit.
- **Tests and test data:** local test pages and public practice sites, made-up credentials. Exceptions (§13): logged-out visits to the §13 consumer sites for the block matrix and read-only public-site tasks; and real-account logins only on a site the chef has himself added to the vault and explicitly asked to test, read-only (no purchases, sends or account changes). Never the chef's real Chrome or Safari profile, real Keychain items, `~/.latch`, `.dev/latch-prod`, or the Pi's `~/latch-dev/state` except where a chunk needs a dev host there. The jev key is read from `/Users/plucas/.claude-kitchen/plow-latch-cli/secrets/jev-api-key` into the host's state dir for tests, never printed; spend under $2 per chunk.
- **Host:** the Mac dev host may be restarted for checks. The Pi has exactly one dev build in `~/latch-dev/`; `~/.local/bin/latch` and `~/.latch` stay empty.
- **Global installs** are avoided; any unavoidable one is appended as a new row to `/Users/plucas/.claude-kitchen/plow-latch-cli/notes/global-installs.md`.
- **NEVER git push or open a PR.** Commit locally on `latch-cli`, one or more commits per chunk.
- **Stop and report** (`NEEDS_CONTEXT`) rather than weaken a rule above to make a chunk pass. §14 open question 1 is the known one: on macOS it is settled (engines run outside latch's Seatbelt, §4); on Linux, if an engine's own sandbox can't run inside bubblewrap, stop for the chef. Never disable an engine's own sandbox.

### Chunk B1: CfT launches from the macOS host with its own sandbox on
Implements: §4 (browser worker, plugins, profiles), §8, §10 launch · close and screenshot, §12 (CfT pin and install) — macOS only; answers §14 question 1 for macOS.
Interfaces: consumes the host, worker and plugin machinery of the latch POC · produces the long-lived browser worker, the CfT plugin package, profile store, and the driver's launch/close/screenshot that every later chunk uses.
Done when: on the Mac dev host, a host-side test launches pinned CfT headed from the host (outside latch's Seatbelt, per §4) over a CDP pipe with a minimal environment, with Chrome's own sandbox on (evidence from Chrome's process flags or sandbox status), sha256-checked binary, loads a local page, writes a screenshot, and closes with no child left; a check from inside `exec` shows no listening debug port and no readable profile.

### Chunk B2: open, observe, act by ref, sessions
Implements: §5 (open, observe, text, screenshot, act by ref, tabs, dialogs, sessions, close, element list, results, exit codes), §10 tabs/frames/run latch's script/resolve/input/wait, §11 base records.
Interfaces: consumes B1 · produces the shared core, page reader, snapshot/ref model and `latch browser` client commands that B3–B7 extend.
Done when: from a client on another machine (or the Pi as client) against the Mac host: open a local multi-frame test page and a public site, observe returns the element list with row text and cross-site frames with effective origins, act by ref clicks/types/selects with trusted input, a stale ref is refused with `stale_snapshot`, a screenshot file arrives on the client, and audit lines exist for each call with no page text.

### Chunk B3: eval, files, and the raw-needs log
Implements: §5 eval, upload, downloads, download get; §11 eval record, workaround tags, `unsupported_capability` log.
Interfaces: consumes B2 · produces eval and file transfer for agents; the audit evidence for §14 question 8.
Done when: eval returns a JSON result from a local page and is audited verbatim (≤ 4 KiB) with workaround tags on a test script that patches `fetch`; an upload and a completed download round-trip as files with matching sha256; an unsupported capability returns `unsupported_capability` and is logged.

### Chunk B4: the vault, the login step and fill parity
Implements: §9 (backend interface, own store with data key + 0600 custody key, four item kinds, revisions, owner commands over latch's connection, import), §7 login, fill and fill-code, §10 prepare/checked fill and pause, §11 fill records.
Interfaces: consumes B2 · produces the vault backend interface later backends implement, and `login` / `fill` / `fill-code` for agents.
Done when: a login item added from a client logs in to a public practice login site via `login`, including on a subdomain of its site; it is refused (`origin_mismatch`) on a lookalike domain, on `http`, and on a sandboxed-iframe test page; a card item fills a local checkout test page with `fill … --press` (number, expiry reshaped, CVV) and the card fields are gone before the agent resumes, while a secret-field fill without `--press` is refused; an identity item fills a local address form; `eval` is refused while a value is on the page; screenshots and observe return `session_paused` during fills; a TOTP item fills a local TOTP form and the code matches RFC 6238 vectors in unit tests; a 1Password 1PUX and a Chrome CSV fixture import through the picker; the ported classifier truth table passes; a grep of results, audit and logs finds no value; exec and file workers can't read the vault or its key; the audit flags `agent_code_before_fill` when eval ran first and records every card fill's site.

### Chunk B4b: signed latch on macOS, Keychain half, Latch app import
Implements: §9's macOS Keychain half of the custody key and import from the Latch app's vault. Blocked on the chef: an Apple App ID with keychain sharing, a provisioning profile, and where the Developer ID certificate lives.
Interfaces: consumes B4 · produces a signed, notarized `latch` in a minimal app bundle.
Done when: the bundled `latch` is signed and notarized; the custody key is split between the state directory and a data-protection Keychain item ("this device only"); a copy of the state directory on another account can't open the vault; a sandboxed `exec` worker can't obtain the Keychain half (including by running the signed `latch`); `latch vault import --from-latch-app` imports a throwaway Latch-app vault through the picker without writing to the app's files.

### Chunk B5: several steps per turn and the decision model
Implements: §6 (targets, expectations, step lists, risky clicks, swappable model interface with jev, no-key behaviour, metric fields).
Interfaces: consumes B2 (B4 for the login-wall stop) · produces descriptions, expectations, `act --steps`, `find "<description>"`, `wait --for`, and the decision-model interface.
Done when: client-generated request ids and `act --history R` return the per-step trace after a reply is dropped mid-list (forced in a test); on a local multi-page test site, one `act --steps` request completes a ≥ 6-action task with descriptions and typed + sentence expectations; a staged surprise stops the list with a compact element list and a trace; a vague expectation is refused; a "Place order" click by description on an unseen page stops with `confirm_target`; with no key, descriptions resolve by exact name only and sentences return `assist_off`; the audit carries the turn/parent-pid fields and model-call metadata without content.

### Chunk B5b: decision model through Plow inference
Implements: §6 product path — the host calls the decision model through a Plow inference route with its own Plow credential; no model key on hosts. Blocked on a Plow API route (to add to the pending Plow API PR alongside the offline and take-over routes).
Interfaces: consumes B5's provider interface · produces a `plow` provider and removes the need for a host key file.
Done when: with no key file on the host, B5's Done-when tasks pass using the `plow` provider against a dev Plow stack; the host's audit records provider `plow` and the model; the key file path is only used when explicitly configured for development.

### Chunk B6: handoff and screenless hosts
Implements: §7 handoff (Mac window, dashboard notice, `handoff done`, refs invalidated, `no_screen` fast fail), §8 Xvfb displays.
Interfaces: consumes B2, B4 · produces the owner handoff flow.
Done when: on the Mac a handoff pauses the agent, the dashboard shows it, `latch browser handoff done` resumes it and old refs are refused; with the display check forced to "no screen" in a host test, the call returns `needs_owner` with `no_screen` at once (the real screenless check is B7's).

### Chunk B7: Linux and the Pi
Implements: §8 Xvfb per session, §12 Linux install (CfT linux-arm64, Xvfb), the bubblewrap browser worker; answers §14 questions 1 and 3 for Linux.
Interfaces: consumes B1–B6 · produces a working Linux/Pi browser host.
Done when: on the Pi dev host (`~/latch-dev/`), CfT runs headed on a private Xvfb display inside bubblewrap with Chrome's sandbox on; a client on the Mac opens a public site, observes, acts, and receives a screenshot from the Pi; a vault login works there with the 0600 key; `handoff` returns `needs_owner` with `no_screen` at once; the exec-side port and key checks pass. If CfT linux-arm64 fails, stop and report before switching to Debian Chromium.

### Chunk B8: Camoufox as the second engine
Implements: §10 Camoufox engine (Node helper, pinned runtime, JSON-lines driver ops), `install.sh --with-camoufox`; the conformance suite as a reusable runner for any driver.
Interfaces: consumes the driver contract from B1–B6 · produces the second engine and the conformance runner the bake-off uses.
Done when: the conformance runner passes every test for CfT, and every non-fill test for Camoufox on macOS (if Camoufox fails only fill tests, `login`/`fill`/`fill-code` are disabled for it and the failures are listed; any other failure means not done); the B2 and B5 Done-when tasks pass unchanged with `--profile` bound to a Camoufox profile.

### Chunk B9: agent guide and an end-to-end run
Implements: §12 agent guide and `latch browser --help`; §6 metric reporting.
Interfaces: consumes B1–B8 · produces the guide agents read and the first actions-per-turn numbers.
Done when: (a) a fresh Claude Code agent, given only the guide, does three tasks on public/test sites through a Mac host (one needing a vault login, one multi-page with a download) twice: once with the full commands and once restricted to refs-only (no descriptions, sentences or step lists); a small report from the audit gives, per task and arm, success, actions per request, actions per agent turn (by `--turn`), and any `eval`/`unsupported_capability` escapes. (b) The §13 test set runs from one command against a host and engine, writes a report (task → pass/fail with evidence; security cases → outcome), and the consumer-site matrix is filled for CfT and Camoufox on the Mac (two runs, different days), with screenshots.
