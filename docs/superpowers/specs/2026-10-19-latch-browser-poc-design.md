# latch browser proof of concept — design

Date: 2026-10-19 · Owner: head chef · Stage: **proof of concept on real hosts, on the path to the real product**, the same bar as `2026-10-14-latch-cli-poc-design.md`.
- It runs on owned test accounts and public test sites.
- It is not throwaway: the code must be clean enough for later work to build on directly.
- Reviewers don't demand the hardening that §2 defers.

Source of truth: `browser-api-proposal.html` (kitchen notes) and the chef's browser and vault rulings of 2026-10-19. Plans, task sessions and vault edit: `plan-merged-rec.md` (chef-approved 2026-10-19), from `design-plan-{jplan,bapix}.md` and their critiques. The one `fill`: `brief-fill-go.md` (chef-approved 2026-10-20), from `design-fill-{fillr,bapix}.md` and their critiques.

## 1. Goal

An agent drives a real browser on a latch host through `latch browser …`. Each call is one exact action, or one plan of steps. The browser stays open on the host between calls. The POC shows, end to end:

- **The same agent commands on two engines,** Chrome for Testing (CfT) and Camoufox. Each plugs in as a driver behind one shared core; the plugin model is also the bake-off.
- **A numbered element list the agent acts on by ref**, plus page text and screenshots, on every host, a screenless Pi included.
- **Plans.** The agent writes a short plan: steps, the text they may type, and what each step should lead to.
  - A swappable decision model (jev in the POC) carries out each step on the live page. It chooses operations and targets; it never writes text.
  - latch brings back where the plan got to: `reached`, or stopped at a step with a reason. Either way the answer carries the evidence for each step, the full trail (pages, actions, screenshots) and the current page.
  - Exact actions on refs stay available for confirmations and repairs.
- **One vault per latch install, shared by every engine.**
  - A saved login is filled only on its own site or a subdomain of it (the same registrable domain, HTTPS; §7 step 4), in one checked step, with the agent paused.
  - No command returns a stored secret.
  - Authenticator codes are computed on the host.
  - Texted codes, CAPTCHAs and passkeys go to the owner through a handoff.
- **Everything goes through the host's gate and audit,** with no secret in any log, result or error.
  - Each `open` makes a new **session**, reachable only by its caller and only with its unguessable id.
  - Sessions share a profile's logins, never each other's tabs, downloads or trails.

## 2. Scope and non-goals

In scope: §1, on macOS hosts first and Linux hosts (the Pi) second. Not in the POC:

- **Raw browser commands** (raw CDP, Playwright scripts). `eval` stays, and latch logs what would have needed raw (§11).
- **The owner's remote view** of the host's browser. It is the direction for screenless hosts, but not this POC (§7, §8).
- **The owner's own Chrome or Safari** as a lane. It is a nice-to-have, a later bake-off lane.
- **Per-site consent or other privacy machinery** for the decision model. The model and its terms are picked later.
- **The gatekeeper** (script review, per-use approval of fills), **and Windows hosts.**
- **Also not:**
  - CloakBrowser;
  - network interception, extensions and closed shadow DOM;
  - passkeys beyond handoff;
  - an autofill engine: fill suggestions come only from standard field hints, with no keyword classifier of fields or buttons (§7);
  - **a free-goal runner** (latch never invents steps; the agent writes them);
  - a generative text model;
  - loops or branches inside plans;
  - coordinate clicks;
  - cross-host site reservations;
  - runner-issued task identity (all four come later; §15);
  - other vault backends and secure-link UX;
  - signed agent identity;
  - stealth work beyond the pinned launch recipe.

## 3. Vocabulary

- **Engine / driver:** the browser program, and the code controlling one family of engines.
- **Profile:** a latch-owned browser data folder, bound to one engine for life. It holds logins, cookies and site storage.
- **Profile runtime:** the one running browser on a profile, with its worker and, on Linux, its display. It is started by the first session that needs it, and it ends when the profile's last session closes.
- **Session:** one task's share of a profile runtime.
  - Its own background window, the tabs its actions open, its snapshots, downloads, plans and trail.
  - Reachable only by the caller that opened it, and only with its id (§5.1).
  - There is **one kind of session**: element actions, plans, agent scripts (`eval`) and vault fills mix freely.
- **Snapshot / ref:** one reading of the page (`o3`) and an element in it (`e7`). Valid only in that document of that session.
- **Plan / step:**
  - A plan is a request to the plan command to run up to 10 steps in order.
  - A step is either an **instruction** (a goal in words, carried out by the decision model) or a **literal step** (one exact action on refs).
- **The plan command:** `latch browser do`. Its name is pending the chef, who asked "why not `steps`?". Only this entry and the §5 command table spell the name; all other text says *the plan command*, so a rename is two edits.
- **Check:** what a step must lead to.
  - A typed check is decided by latch's code.
  - A sentence check is judged by the decision model, with a quoted line of the page as evidence.
- **Confirmation:** the agent's explicit go-ahead for one consequential action. It is bound to the exact element and to what the page showed about it.
- **Item:** a saved login, card, identity or note in the vault.
- **Mapping:** which page element gets which item field in a fill, plus the button latch presses after it. The agent writes it, or accepts a **suggestion**: a mapping latch built from the page's own hints, with an id and the evidence for each element (§7).
- **Decision model:** a model that only chooses, picking one option from a list or answering yes/no with a probability. It never writes text.

## 4. Architecture

```
agent machine                         latch host
┌──────────────────────────┐          ┌──────────────────────────────────────────────────┐
│ agent ─shell─▶ latch      │◀═ iroh ═▶│ latch serve: verify caller ▸ gate ▸ audit         │
│  browser … (client)       │          │   ├─ vault (one per install, backend interface)   │
└──────────────────────────┘          │   ├─ decision model client (key on host)          │
                                      │   └─▶ browser worker (sandboxed, one per runtime)  │
                                      │        shared core ─▶ engine driver ═pipe═▶ engine │
                                      └──────────────────────────────────────────────────┘
```

- **Client and transport are unchanged.** `latch browser` subcommands are ordinary client commands: `--host`, `--state-dir`, iroh, one stream per operation, the same exit codes. Screenshots and downloads stream to the client as files; uploads stream from it.
- **The host:**
  - gates every call;
  - holds the vault, the decision-model credential, the audit log and each session's trail;
  - **runs plans**: the decision loop is host-side, and calls the worker one browser operation at a time;
  - starts one **browser worker per profile runtime** and talks to it over the worker's stdio.

  Within a session, requests are serialised. Sessions on one runtime interleave their browser operations, so one session's plan or wait never holds the browser for the others. Waits poll from the host. The host never holds the worker while it waits for the decision model.
- **The browser worker** is long-lived for its profile runtime, unlike file and exec workers. It keeps each session's windows, tabs, snapshots, downloads and dialogs apart.
  - **macOS (chef, 2026-10-19):** macOS can't nest a different Seatbelt profile inside another, and every Chrome/Firefox helper applies its own (verified in B1's probe). So the host starts the engine **outside latch's Seatbelt**, with the engine's own sandbox on for every web-content process. What still holds: private pipe only, no port; profile in the state directory, hidden from exec and file workers; pinned binary checked by sha256; a minimal environment with no host secrets. What's lost: latch's confinement of the engine's main process. A separate macOS user for engines is the later hardening.
  - **Linux:** bubblewrap is applied before the engine starts, if the engine's sandbox nests inside it (B7 proves it). The runtime sees:
    - its own profile folder (read-write);
    - its engine's plugin folder (read-only);
    - one scratch folder per runtime, with a subfolder per session;
    - outbound network.

    It sees nothing else of the state directory, and no areas.
  - File and exec workers never see profiles.
- **The shared core** runs in the worker and is engine-neutral. It owns tabs, the page reader (an isolated-world script that builds the element list), snapshots and refs, actions, waits, the fill step and the pause. Secret filtering is written once, here.
- **Drivers** speak each engine's own channel over **private pipes only**, so no other latch worker can connect to a browser.
  - **CfT:** CDP over `--remote-debugging-pipe`, through a driver written in Rust and built into latch.
  - **Camoufox:** a small Node helper running Camoufox's pinned Playwright runtime, which speaks the driver operations as JSON lines over stdio.
- **Plugins.** Each engine installs to its own versioned plugin folder under the state directory, beside older versions, like gog. It holds the build and a manifest: engine, version, driver-contract version, binary and sha256 per OS and architecture, launch recipe, optional capabilities and licence notes. Pins are compiled into latch; the sha256 is checked before every launch.
- **Profiles** live in the state directory.
  - Each `open` creates a new session: a new background window in the profile's runtime, starting the runtime if needed. It never reuses another session.
  - Profiles are created with password saving and autofill off, and no extensions.
  - A session closes after 30 minutes idle. Closing a session never touches the others. The runtime, its worker and its display exit when the last session has closed; the profile stays.

## 5. Agent commands

Every command below is `latch browser <cmd> --host H`; session commands also take `--session S`. Results are one JSON object on stdout with an `audit_id`; errors are `{"error":{"code","message",…}}` on stderr.

| Command | Does |
|---|---|
| `open [--profile P] [--task LABEL] [--reserve-site] [URL]` | Creates a new session in P's runtime (no `--profile`: profile `default`, CfT, created on first use). Returns the **session id** (unguessable; §5.1), engine, URL, title and a compact element list. `--task` is a label for listings and audit, never authority. `--reserve-site` is described in §5.3. |
| `observe [--tab T] [--scope eN]` | A snapshot of the element list, short page text, and what a fill could use (§7): the vault items that fit this page with the fields each holds, what the page asks for, and suggested mappings. Never a value. |
| `text` · `screenshot --output F` | Page text, or an image file on the client. |
| `do STEP… [--fact NAME=TEXT]… [--item I]… [--within S] [--reserve-site] [--output F]` · `do --plan-file FILE\|-` · `do --dry-run …` | **The plan command** (§6). `--plan-file` takes the canonical plan object. `--dry-run` prints the canonical plan and runs nothing. `--output F` writes the final page's screenshot to F on the client, as `screenshot --output` does. |
| `do --confirm C [STEP…]` | Carries out confirmation C from a `confirm_needed` stop, then any further steps (§6.5). |
| `do --history R` · `do --history R --shot N --output F` | A past request's steps, actions, outcomes and page lines; or one of its screenshots (action N, or `final`), streamed into F on the client the way `screenshot --output` and `download get` stream (§6.7). This is how exit 5 is resolved. |
| `act click\|type\|select\|press\|scroll\|back eN [text] [--check CHECK]` | One literal step on a ref from the last page latch returned to this session. It runs through the plan executor as a plan of one step, with the same checks, confirmations, trail and answer. |
| `find --role R --name N` | Candidate refs by role and exact name. No model, never acts. |
| `wait --ref eN --state S` · `--text T` · `--url-path P` | Waits for a typed condition, with a deadline. |
| `fill --snapshot O --item I (--field F=eN… \| --suggested S) [--press eN]` | Fills any vault item: a login, an authenticator code (`otp`), a card, an identity or a note (§7). |
| `handoff --reason captcha\|2fa\|passkey\|signin` | Pauses the agent until the owner says done (§7). |
| `upload eN FILE` · `downloads` · `download get D --output F` | Files move as files. |
| `dialog accept\|dismiss` · `tab new\|switch\|close` | Dialogs and tabs. |
| `eval --script-file F` | Agent JavaScript in the page, in any session. Refused while a filled secret is on the page. |
| `sessions` · `close` | The caller's sessions, without their ids (§5.1); or end the session named by `--session`. Closing ends the session's windows and releases its site reservation. The profile, the runtime's other sessions and its display stay. |

These forms are removed:
- `act` with an exact name or a description as target;
- `act --expect "<sentence>"`;
- `act --steps`;
- `act click --at X,Y`, until it gets the binding listed in §15;
- `find "<description>"`;
- `wait --for "<sentence>"`;
- `login` and `fill-code`, folded into `fill` with no aliases (§7).

Using any of them is a usage error: an unknown argument, or (for a target that is not a ref) a message pointing to the plan command, or (for `login` and `fill-code`) one naming `fill`.

**Owner-device commands.** `latch vault add|edit|list|remove|import` (§9) are client commands run from an **owner device**: a latch client state (its own `--state-dir`, logged in with `latch login` as kind `device`) distinct from the serving host's state. On the host's own machine the owner uses that separate client state, never the host's (the client already refuses to dial its own serving identity). The host accepts them only from a `device`-kind peer on its account; agent peers get exit 4.

**Owner-only, host-local commands.** These need the host's terminal and state directory, so `exec` can never reach them:
- `latch browser profile create|list|remove`;
- `latch browser handoff done`.

**Element list.** Each element carries its ref, role and accessible name; the value of ordinary fields (never password or secret-marked ones); checked, expanded, disabled and off-screen state; and **its row or card text**, which the decision model needs. Off-screen elements are kept. The list is capped at 200 elements, with `truncated` set; `--scope` reads a subtree. Cross-site frames are included with their effective origin. A stale ref is refused, never re-guessed.

**Results.** Every answer from the plan command and from `act` is one JSON object.
- **`status`:**
  - `reached`: every step ended `checked`, `model_assessed` or `already_held`;
  - `dispatched`: every step ran, but a literal step without a check was only sent;
  - `stopped`: with `stop: {step, code, message, …}`.
- **`review: true`** when the last step rests only on a model assessment.
- **`steps`:** per step, `outcome` (`checked`, `model_assessed`, `already_held`, `dispatched`, `stopped`, `not_run`), its evidence (the check that held, or the model's p and quoted line), and its action count.
- **`trail`:** per action:
  - page URL and title;
  - operation;
  - target (ref, role, name, row text);
  - how it was chosen (`ref` or `model`, with the p of each head);
  - the risk p, for clicks and Enter;
  - the result (`dispatched`, `refused`, `unknown`);
  - whether the page changed;
  - a screenshot handle (`{"request": R, "shot": N}`).
- **`page`:** the page now: snapshot id, URL, title, up to 40 main on-screen lines, the compact element list.
- **`screenshot`:** the final page's handle (`"shot": "final"`). With `--output F`, the client path F it was written to.

**A result never contains a path on the host.**
- Every screenshot is a handle, which the client fetches with the plan command's `--history R --shot N --output F` (§6.7).
- The only paths are ones the client itself wrote.
- This works the same from the host's own machine and from another one.

latch never reports "done" for the user's task. `reached` means every step's own check passed, with the evidence shown.

| Exit | Meaning | Codes |
|---|---|---|
| 0 | `reached` or `dispatched` | — |
| 1 | stopped | `check_not_met`, `unconfirmed`, `already_true`, `needs_text`, `needs_fields`, `needs_login`, `needs_owner`, `blocked`, `stuck`, `repeat`, `no_progress`, `ambiguous_target`, `step_budget`, `plan_budget`, `deadline`, `stale_snapshot`, `covered_target`, `session_paused`, `site_busy`, `model_unavailable`, `request_too_large`, `no_such_session`, `unsupported_capability`, `timeout` |
| 2 | usage | bad arguments, a malformed plan, `vague_condition`, a removed form, unknown host |
| 3 | host unreachable; nothing sent | — |
| 4 | refused | `confirm_needed`, `confirm_stale`, `step_not_allowed`, `not_owner`, `origin_mismatch`, `bad_target`, `secret_on_page` |
| 5 | outcome unknown | read the plan command's `--history R` before anything else |

`model_unavailable` replaces B5's `assist_off`.

### 5.1 Ownership

**Every session call needs both:**
- the client key the gate has verified, matching the key that opened the session; and
- the session's id.

**The id is a capability.**
- `open` returns it once. It is at least 128 bits of randomness, and never derived from the profile name.
- Session commands take it as `--session S`, or `LATCH_BROWSER_SESSION`. There is no implicit `default` session.
- `sessions` lists the caller's sessions **without their ids** (task label, profile, current site, state, last activity). So another task under the same client key can't reach a session whose id it was not given.
- The audit and dashboard show a short hash of the id, never the id.

**How a call is refused:**
- A call with an id unknown under the caller's key is refused with `no_such_session`.
- A call with a valid id from another client key is refused with `not_owner` (exit 4). Nothing reaches the session either way.
- An owner device may close any session from the dashboard or by its hash.
- A task that loses its id loses the session, which idles out.

**Limits:**
- **Task identity proper, issued by the agent runner, is a later upgrade** (§15). Until then, one client key plus the session id is the ownership rule.
- The interim `profile_busy` guard retires with B2b: another caller's `open` now gets a session of its own.

### 5.2 What a session owns

Its own windows, plus every tab or popup opened from them (assigned by opener). A target whose opener is unknown stays unassigned until it is resolved; it is never adopted from whichever window is active.

It also owns:
- **its snapshots and refs;**
- **its downloads:** files are named by download guid in the runtime's staging folder, and assigned to a session through the initiating frame → target → session. A download that can't be assigned is quarantined, never shown to anyone;
- **its dialogs,** handled only through their own target;
- **its plan requests and trail.**

### 5.3 The same site in two sessions

Two sessions on one profile share the site's account state: one cart, one login, one set of settings. latch can't separate that.

**By default it warns:**
- While another live session of the same profile has acted on the same registrable domain in the last 15 minutes, every answer on that site carries `shared_site` (the other session's id hash and how long ago it acted, never its pages).
- A `confirm_needed` stop repeats the warning.
- Signing out is a consequential click (§6.5).

**Opt-in lock (`--reserve-site` on `open`, or on the plan command):** the session reserves (profile, registrable domain) on this host, for tasks that change an account (a cart, an order, settings).
- The reservation holds across stops and confirmations, until the session closes (`close`, or the idle timeout).
- Another session's action on that site, navigation included, gets `site_busy` (exit 1) before anything is sent.
- **Limit:** the lock is host-local. It doesn't cover another host, or another profile signed in to the same account. Cross-host reservations need a Plow API (§15).

## 6. Plans and the decision model

### 6.1 The plan

**The canonical plan object** is what `--plan-file` takes, and what every short form compiles to:
- `steps`: up to 10. Each step has one of:
  - a `goal` in words, with optional `values` (named literal texts), `urls` (named absolute URLs) and a `check`;
  - a `literal` action on refs: `click`, `type`, `select` or `press` (`op`, `ref`, `snapshot`, optional `text`), with an optional `check`;
  - a `literal` **`fill`**: `item`, and either `fields` (item field → ref, a list of refs for split boxes) with `press` (the button), or `suggestion` (a suggestion id from a page latch returned to the session), with an optional `check`. This is §7's `fill`, carried inside a plan.
- `facts`: named texts a goal step may type where it names them (`{NAME}`, below).
- `items`: vault item handles the plan may use. Naming an item here is the agent's go-ahead for jev to choose one of latch's suggested mappings for it (§7, inside plans); a plan that names none never fills by itself.
- `within`: seconds, default 120, at most 300.

The whole plan is validated before anything runs (shape, step count, refs, URLs, fill mappings, check syntax). A malformed later step never lets earlier steps run.

**The short form** is the plan command with one argument per step:
- **Values.** Text in double quotes (straight or curly) is a value of that step.
- **Fact references.** `{NAME}` outside quotes names the plan's fact NAME: its text joins that step's values. A step types only its own values: its quoted texts and the facts it names. A name the plan has no fact for is refused before anything runs. Nothing else is ever typed.
- **URLs.** A quoted absolute URL is also offered to `navigate`.
- **Checks.** A step's check follows its **last** ` -> ` outside quotes. So `add the note "A -> B"` has the value `A -> B` and no check.
- **Literal steps.** A step that matches either of these is a literal step:
  - `click|type|select|press eN ["text"]`;
  - `fill --item vN (--field F=eN… [--press eN] | --suggested S)`.

**Checks:**
- `origin: https://www.doordash.com`: the page's origin equals it.
- `path: /checkout`: the value is part of the page's path. Never the host, never the query.
- `query: q=salad`: the query has that parameter with that value.
- `text: Added to cart`: the page text contains it.
- `field: eN=<value|checked|unchecked>`: that field's state, by a ref from a page latch returned.
- `download: complete`: a download started by this step has completed.
- `new text: …` (or `"require_change": true` on any check): it must not hold when the step starts, and must hold after.
- `<sentence>`: judged by the decision model (§6.3). A sentence that names a task rather than a page fact is refused before anything runs (`vague_condition`).

Checks wait up to 10 s after a step's last action. In an instruction step, a typed check is also polled for up to 2 s after each action, before the next decision (most of a step's actions are on the way, so the full 10 s is kept for when the step stops after acting).

**Step policy: none until B9.** Every literal step on a ref latch returned to the session runs. B9's agent adoption test adds a host step policy for its arms (`any`, `stop`, `none`), and decides whether one ships.

### 6.2 One pass of an instruction step

Each pass observes the page and makes **one decision-model request**. In that request:
- every question restates its own operation, because questions are answered independently;
- only operations and targets that are possible now are offered.

**The state sent:**
- the plan, with each step marked done, current or later;
- the current step and its values;
- the page's URL and title;
- up to 40 on-screen lines;
- **field states as lines** (`combobox "Size": "Large"`, `checkbox "Gift": not checked`);
- the scroll position;
- the actions already taken in this step;
- the lines new since the step began: written on screen since then (a line whose text was set again counts, even with the same words, such as a message an add-to-cart writes again), or text that was not on the page then;
- the vault items that fit the page (title, kind, the fields each holds; never a value), and what the page asks for (§7).

**The questions:**
- **status:** `complete` (the page or the actions taken show the step's result), `not_yet`, or `cannot` (an error, an unavailable item, or missing information).
- **evidence:** the one line that best shows the step is complete, or `none`. The options are the page's title and address, its on-screen and other lines (up to 100), its field-state lines, and this step's action lines.
- **operation:** among only those possible now:

  | Operation | Offered when |
  |---|---|
  | `click`, `type`, `select` | there is a target for them |
  | `press_enter` | after typing |
  | `navigate` | only to the step's or plan's URLs |
  | `scroll`, `back` | always, where they apply |
  | `wait` | until the step has waited twice |
  | `fill` | a vault item fits the page under its site rule, and the page shows sign-in or form fields (§7). Never on a code or passkey stage: a code stage stops before the decision, and so does a passkey stage unless a named fitting item holds a password (§6.4 stop 1) |
  | `done` | only after the step's first action, and only in a step without a typed check (latch decides that step's completion) |
  | `stuck` | always |

  Choosing `fill` never makes jev map fields: it picks one of latch's complete suggestions or `none` (the targets below, and §7).
- **targets:**
  - click targets: elements an action can reach now, not disabled and **not covered**. Each with its row or card text, off-screen ones included, at most 250, with truncation shown.
  - field × value pairs over the step's own values, **always with `none`**, even when there is only one pair. With no value to pair, the fields text could go into, so a `needs_text` names the field the step wants.
  - dropdown × option pairs, **including the option already chosen**, marked as such.
  - supplied-URL ids.
  - fill bundles: latch's complete suggestions for the items the plan names, each shown as the item's title and kind, its map (item field → the element's name on the page) and its press button, **always with `none`**, even when there is only one.

  Every model-chosen field × value pair and fill bundle keeps its `none`.

  **Size.** A request that would pass the route's 64 KiB is cut to fit (latch budgets 60 KiB): the page's other lines first, then click targets from the end of the page, then field and dropdown pairs, then on-screen lines, then field lines and the step's action lines. The request says what was left out, and so does the action's trail entry. What is never cut (the plan, the step's own values, the page's address and title) may still not fit: then the step stops `request_too_large` and nothing is sent.

### 6.3 When a step is complete

| How | Rule |
|---|---|
| **Typed check** | It decides. The step is `checked` when it holds after the step's action. |
| **Sentence check, or no check** | The step is `model_assessed` when status is `complete` at p ≥ 0.7, **and** an evidence line is chosen with p(`none`) < 0.3. It is reported with p and the line. |
| **Already held** | Before its first action, a step whose state check already holds ends `already_held`, with no action. Without a typed check, the model's `complete` counts here only when its evidence line is a **state line**: a field's value, the page's address or its title. Page text alone, such as a flash message left by an earlier step, is never enough. A step with `require_change` can't be already held. |
| **`unconfirmed`** | The step has acted, the model chooses `done`, and the rule above isn't met. It comes with the top evidence lines. |
| **`already_true`** | The model chooses `stuck` before acting, while the step's result shows only in page text. |
| **`blocked`** | Status `cannot`, with the evidence line. |
| **`check_not_met`** | A typed check that didn't hold. |

**Thresholds, wording and budgets are starting values, frozen per evaluation.**
- **Two kinds of cases:**
  - *development* cases: B5c's plan suite, and anything used to choose or change a value;
  - *held-out* cases: B9's evaluation set, disjoint in tasks, page layouts and sites.
- **Held-out cases become development data once they inform a change.** If B9's results lead to changing wording, a threshold or a budget, B9's cases count as development data from then on. Claiming an evaluation of the revised design needs a fresh, disjoint held-out set.
- B9 reports false accepts and false stops separately.

### 6.4 Stops and limits

Each pass applies these stop conditions, in this order:
1. Pages latch can't go past by itself, whatever operations are offered or chosen, and whether or not any vault item fits:
   - a CAPTCHA page: `needs_owner`;
   - a **code stage** (a `one-time-code` field): `needs_fields`, with the payload in 9 (the fitting items may be none). latch can't tell a texted code from an authenticator code, so the agent chooses between a literal `fill` of `otp` and `handoff`;
   - a **passkey stage** (a `webauthn` token, and neither a password field nor a `username` token: an identifier field marked `username webauthn` is an identifier stage, not a passkey stage): `needs_fields` as above, **unless an item the plan names fits the page and holds a password** (chef, 2026-10-20: passkey-first sites usually also take a password). Then the pass goes on: the stage offers no fill bundle, and jev may click the page's way to its password path ("Use your password instead"), whose page gets its own `fill`. latch holds no passkeys, so with no such item the agent hands off.

   A code field with no `one-time-code` token is not detected here; no step value fits it, so typing into it stops `needs_text` (7).
2. Completion as in §6.3.
3. `cannot` stops the plan with `blocked`.
4. `done` stops the plan with `unconfirmed`; `stuck` stops it with `stuck` (or `already_true`).
5. Budgets:
   - 10 steps;
   - 8 actions per step;
   - 30 actions per plan;
   - `within`;
   - at most 2 waits per step;
   - exhausted budgets stop with `step_budget`, `plan_budget` or `deadline`.
6. A chosen target below p 0.5: `ambiguous_target`, with up to 3 candidate refs. There is no floor on the operation head.
7. Typing whose field × value choice is `none`, or where none of the step's own values fits: `needs_text`, naming the field. A fact the step does not name is not one of its values, so it is never typed there.
8. `fill` chosen on a sign-in page when the plan names no fitting login item: `needs_login`, with the fitting items (value-free) and the page's suggestions.
9. `fill` chosen with bundle `none`, or with no complete bundle for an item the plan names: `needs_fields`, with:
   - the fitting item handles and the fields each holds;
   - the form's candidate refs (its fields and buttons);
   - the item fields each one could take;
   - what the page asks for, and its other sign-in controls as refs.

   The agent sends a literal `fill` step with its own mapping, or names the item, in a new plan.
10. **Repeat guard:**
    - in a step **without a typed check**, each control is clicked or submitted at most once; a second try is refused with `repeat`, nothing sent (the control stays on offer). Whether the first try did the step is the model's to judge, and if it can't establish that, the step stops `unconfirmed`. With a typed check, the same control may be used again until the check holds;
    - an action identical to one already dispatched in this plan, on the same page state (same document, field values, on-screen lines and scroll), is refused with `repeat`;
    - two actions in a row that leave the page state unchanged stop with `no_progress`.

    The same control on a changed page is real progress in a step with a typed check, as in a same-URL wizard (`-> text: All set`), and is allowed.
11. A consequential action, or a fill bundle that releases a card or identity secret: `confirm_needed` (§6.5).

Then:
- **Before dispatch,** latch re-checks:
  - the target's identity and nearby context in its document;
  - the session's ownership;
  - the profile's pause epoch: a decision made before a pause is thrown away, and the page is observed again;
  - the site reservation.
- **Dispatch** uses trusted input. It is journaled before it is sent and after it returns. A click is never sent twice by latch, and an uncertain action is never replayed.

### 6.5 Confirmations

These stop with `confirm_needed` (exit 4) before anything is sent:
- **a click** that would pay, place an order, send, publish, delete or close something, sign out of an account, create credentials, or otherwise can't be undone;
- **an Enter** in a field whose form submits such an action;
- **a fill that releases a card or identity secret field**, through the plan command (a literal step or a chosen bundle) or the standalone `fill` command (§7).

This applies whether the model chose the target or the agent named it in a literal step, and whether or not the agent has seen the page.
- **The decision.** It is a decision-model yes/no on the exact element (or, for Enter, the form's submit control), at p ≥ 0.5. Enter in a form with no submit control latch can identify is judged as a submit of that form.
- **There is no keyword fallback.** If no decision model is reachable when a risk decision is needed, the step stops with `model_unavailable` and nothing is sent. It never passes silently.
- **An issued, still-valid confirmation dispatches normally,** with no new decision (all hosts reach a model through Plow inference, B5b).

The stop returns **a confirmation**:
- its id;
- the target's ref and snapshot;
- the reviewed context: the target's name and row or card text, the page title, and the lines naming the item, amount or recipient. For a fill, also the item's title, **the exact mapping** (item field → ref, with the element's name on the page and its evidence), **the site of each destination frame**, and the press button;
- a screenshot.

The agent confirms with the plan command's `--confirm C [STEP…]`, for clicks, Enters and fills alike; there is no other route. A click or Enter sent any other way, `act click` on the confirmation's own ref included, gets a new risk decision: if it still looks like it can't be undone, it stops again with a new confirmation, and the stop's message names `do --confirm`.

A confirmation:
- **is used once;**
- **is valid only in the same session and document,** and only while the target and reviewed context are unchanged at dispatch; for a fill, also the mapping, every destination frame's site and the item's revision;
- **otherwise is refused with `confirm_stale`** and a fresh page. A total that changes from $10 to $100 is refused, even with the same ref, URL and button name.

A literal step written into a plan before latch found the consequence is never a confirmation.

### 6.6 Text, the model and its absence

- **Every typed string** comes from the step's values (its quoted texts and the plan's facts it names), or the vault through §7. There is no generative text model.
- **The decision model is swappable:**
  - one host-side interface: choose one option from a list, or answer yes/no with a probability;
  - answers are validated before use: offered options only; finite probabilities that sum to 1 and peak at the choice;
  - **POC:** TypeSafe jev, pinned to `jev-1.13.0`, dev key in a 0600 file. **Product:** Plow inference on the host's credential, with no decision-model key on hosts (B5b).
  - calls run in the host, never in the worker. They send element names, row text, page lines and non-secret field values, and never cookies, vault values, seeds or screenshots.
- **When no model is reachable:**
  - these stop with `model_unavailable`, nothing sent: instruction steps, sentence checks, and any click, Enter or secret fill that needs a new risk decision;
  - these still work: literal `type`, `select`, `scroll` and `back`; typed checks; and confirmations already issued.

### 6.7 History and the trail

Every request has a client-made id, so the client knows it even when the reply is lost.

The host journals each request's steps and actions to the state directory **before and after each dispatch**. The journal survives a host restart:
- an action whose result was not recorded is reported `unknown`, never `not_sent`;
- a missing record never counts as "safe to retry".

**The trail bundle** belongs to the session's owner and is kept in the state directory apart from the value-free audit (§11). It holds:
- page lines and titles;
- a screenshot after each action;
- markers for suppressed secret phases.

It is kept 7 days (a trial value). A screenshot is fetched by request and action through the plan command's `--history R --shot N --output F`. The request's journal and page lines come back as JSON from `--history R`.

**How a client fetches a screenshot.** It is the same path `screenshot --output` and `download get` use today:
- one `browser` request over latch's connection;
- the bytes stream after its first answer into the client's file;
- both sides compare sha256.

Access needs the caller's client key and the session's id. The id stays valid for its trail for the trail's 7 days, even after the session closes.

Two alternatives were considered and rejected:
- **`latch file get`:** file workers can't, and mustn't, read the state directory where trails live (§4, §9). Putting trails in a shared folder would show them to exec workers and other callers.
- **Streaming every screenshot with each answer:** that would need a multi-file transfer latch doesn't have, and would send images the agent never looks at.

### 6.8 Metrics

Measured from the audit, grouped by the caller's turn id (`--turn` / `LATCH_TURN`, with parent-pid fallback):
- **Plan share:** actions from instruction steps ÷ all actions.
- Confirmations, and how they were answered.
- **Continuations:** what follows a stop: a new plan, or literal repairs.
- **Escapes** to `eval`.
- Agent turns and actions per turn.
- Stops by code, including `unconfirmed`.
- Wrong targets, from trail review.
- Model calls and latency, per plan and per action.

B9 sets the targets.

## 7. Fills, codes and handoff

**One command fills every vault kind** (chef, 2026-10-20): logins, authenticator codes, cards, identities and notes.
- **The agent decides where each value goes.** latch suggests a mapping from the page's own hints, checks any mapping before it releases a value, and owns the destination, the submit and the cleanup.
- **latch never places a value nobody mapped.** Outside plans the agent writes the mapping or accepts a suggestion by its id. Inside a plan, jev may choose a suggestion only for an item the plan names.

**`fill --snapshot O --item I (--field F=eN… | --suggested S) [--press eN]`:**
- **`F` is a field of the item,** by its vault field id, never a page label:
  - login: `username`, `password`, and `otp`, a code the host computes from the item's authenticator key (the key never leaves the vault);
  - card: `number`, `expiry`, `cvv`, `name`, …;
  - identity: name, address and identity-number fields;
  - note: its text.

  A list of refs (`--field otp=e5,e6,e7,e8,e9,e10`) fills split boxes in order. latch splits the value, and reshapes dates and expiries to the field's format.
- **`--suggested S`** takes suggestion S from the `observe` that made snapshot O. It is the same mapping object an explicit `--field` list builds, and it goes through the same checks.
  - latch recomputes S at fill time. If O is stale, the item's revision changed, or the recomputed mapping differs, the fill is refused `stale_snapshot`, with a fresh page and its suggestions.
  - latch never remaps, picks another item or finds another button on its own.
- **`--press eN`** is the button latch presses after setting the values. A mapping that holds a secret (a password, a code, a card or identity secret field) needs one, given or inside the accepted suggestion. A fill of non-secret fields only may leave it out.
- **Removed:** `login` and `fill-code`, with no aliases (§5).

**What `observe` tells the agent (never a value):**
- **`items`:** each vault item that fits the page under its site rule (§9): handle, title, kind, revision, and its fields, each marked `present`, and `secret` or `derived` (`otp`) where so. No value, and no username.
- **`page_offers`:** what the page asks for, from standard hints only: `password` (a password field), `username_only` (a `username` token and no password field: an identifier stage, not a promise that a password follows), `code` (`one-time-code`), `passkey` (`webauthn`), `card` (`cc-*`), `address` (address tokens).
- **`suggestions`:** per fitting item, at most one per form: an id, the item and its revision, the snapshot, the map (item field → ref), the press ref, each ref's evidence (`autocomplete=current-password`, `type=password`, `the text field before the password`) and each target frame's site.
- **`alternatives`:** the form's other buttons and links, as refs with their names, unclassified ("Sign in with a passkey", "Continue with Google" are read by the agent, not by latch). Clicking one is ordinary navigation, and the next page gets its own `fill`.

latch holds no passkeys. A passkey-only sign-in is the owner's: the agent runs `handoff`.

**How a suggestion is built.** Only from standard evidence, never from labels or button words:
- valid `autocomplete` tokens, parsed with their section, `shipping` and `billing` qualifiers (`on`, `off` and invalid tokens are ignored);
- native input types;
- one structural rule for one-page logins, as Chrome's password manager has it: a `type=password` field, and the single text or email field before it in the same form;
- a lone field makes a login suggestion only with a `username` token; an `email` token or type alone does not (a newsletter box is not a sign-in);
- the press target: the form's single submit button (a submit-type `button` or `input`). None, or several: no press is suggested, and the agent names one.

No suggestion is made:
- on a form that looks like registration or a password change: a `new-password` token, or two password fields;
- for `otp`: a `one-time-code` hint can't tell an authenticator code from a texted one, so the agent maps `otp` itself;
- where the map would be ambiguous (two eligible elements for one item field) or incomplete. The candidates still come back, and the agent maps by its own reading of the page.

Suggestions and mappings use latch's element list, which drops hidden, `aria-hidden` and inert elements. Checkouts carry hidden duplicate inputs with the same tokens; those are never a target.

**The fill:**
1. **Gate.**
   - No per-profile binding (chef, 2026-10-19): any session may use any vault item, subject to its site rule; who may use what is the later gatekeeper's job.
   - The host notes whether agent code (`eval`) has run **on this site in any session of this profile**. It keeps that with the profile, because service workers and site storage outlive sessions.
2. **Pause.** The whole browser is frozen for agents while a latch-filled secret may be on a page. That means **every session on the profile runtime**: page text, screenshots, observations and actions.
   - Calls return `session_paused`.
   - Plans in other sessions wait at their next action boundary, then observe again.
   - The pause ends only after cleanup is confirmed (step 6). If cleanup can't be confirmed, the whole runtime stays paused.
3. **Check the mapping,** before any value leaves the vault. A failed check returns `bad_target` (exit 4), naming the field and the reason, and releases nothing.
   - **Every target:** in snapshot O and still in its document; visible, enabled, editable and not covered; one item field per element, and one element per item field (ordered split boxes excepted).
   - **`password`:** a `type=password` field, or a text field with a `current-password` token (a revealed password). Never a field with a `new-password`, `one-time-code`, `cc-*` or search role.
   - **`username`:** not `type=password`; not a `cc-*`, `one-time-code` or `new-password` token; not search.
   - **`otp`:** not a password field with a `current-password` token; not a `cc-*`, `username` or `email` token.
   - **Card `number` and `cvv`:** a text, tel or number input (never a textarea, contenteditable, search field or checkbox), with no token naming something else. An unhinted but compatible text field is allowed on the agent's explicit map (chef, 2026-10-20); the confirmation shows that exact map (below).
   - **Everything else:** refused only on a contradiction (`type=password`, or a `cc-*` token for a non-card field). A weak mismatch, such as an email into a `tel-national` box labelled "phone or email", is allowed and flagged `hint_mismatch` in the result and the audit.

   These checks reject what the page's own markup contradicts. They can't prove what a page means: a page can lie with `autocomplete=cc-number`, and an unhinted box can be anything. The origin rule (step 4) bounds who receives a value; the mapping supplies the purpose.

   The driver resolves each target and reports:
   - its frame's **effective origin**, read as `self.origin` in latch's isolated world and bound to the frame and document (a frame URL or CDP `securityOrigin` misreports sandboxed frames);
   - the **top-level origin**;
   - the document id.
4. **Match.**
   - **Logins and codes (chef, 2026-10-19: subdomains match, for parity with the Latch app).** Both the field frame's and the top-level origin must be HTTPS and share the item's **registrable domain** (public-suffix list via `psl`, private suffixes included), unless the item allows other top-level sites.
   - **Cards, identities and notes:** the per-kind rule in §9, unchanged (chef, 2026-10-20: no list of payment providers). A card's field frame may be another site, such as a payment processor's; it is checked, audited and shown in the confirmation.
   - For every kind, plain `http`, an IP or local host, a missing page address, and opaque or uncertain origins are refused (`origin_mismatch`). Only then does the vault release a value.
5. **Set.** One value at a time crosses the worker's private pipe. The driver re-checks element, document and origin and sets the value in one synchronous step, with no typing. A site that rejects it gets `needs_owner`, never a typing fallback.
6. **Submit and clean up.** latch presses the mapping's press target. It waits until the secret-bearing document is replaced, or the values are verified cleared (a URL change isn't enough). On failure it clears the fields. If cleanup can't be confirmed, the whole runtime stays paused (step 2).
7. **A login's second stage (chef, 2026-10-20).** When a login fill maps no password (an identifier stage: a username and its continue button), latch carries on inside the same pause.
   - It waits up to 15 s after the press for the next stage (another document, or new controls in the same one), and reads it privately.
   - It fills the password itself only when all of these hold:
     - the stage asks for a password: exactly one element passes step 3's `password` rule, on a form that isn't registration-like;
     - the origin match (step 4) passes again for the same item;
     - the stage has exactly one submit button, by the suggestion's rule.

     Then it sets the password, presses that button and cleans up as in steps 5 and 6. A fill has at most two stages.
   - **Anything else ends the fill after cleanup**: a code or passkey stage, a method or account chooser, two password fields, no single submit button. The result is `needs_fields` (exit 1), carrying a fresh snapshot of the new stage, its candidates, what it asks for, its other sign-in controls and the item's fields; the first stage is reported `submitted`. A second stage on a site the item doesn't match is refused `origin_mismatch`, with nothing released.
   - **A code stage never continues by itself**, since a code field can't tell an authenticator code from a texted one. The agent maps `otp` in a new `fill`, or hands off.
8. **Resume.** The result is `submitted`, not "logged in" or "paid", with one outcome per field (`{field, ref, outcome: filled|not_accepted, hint_mismatch}`, never a value) and the number of stages. `eval` is refused while a filled secret is still on the page.

**Releasing a card or identity secret needs a confirmation (chef, 2026-10-20).** Secret fields (card number, CVV, identity numbers, per the ported classifier) must leave the page before the agent resumes. So a fill that includes one needs a press button, and it is **confirmed before any value is released** (§6.5).
- The fill first stops with `confirm_needed`, carrying the item's title, the exact mapping, each destination frame's site, the press button and the reviewed amount.
- The confirmation binds that mapping and one release. It lets latch fill, press and clean up as in step 6, as one step. A changed mapping, frame site, item revision or reviewed context is refused `confirm_stale`.

This holds for the standalone `fill`, a literal `fill` step and a chosen bundle in a plan alike. Non-secret fields (name, address) may fill without a press button and without confirmation. Site lists and auditing are in §9.

**Inside plans.**
- **The `fill` operation** (contract in §6.2).
  - The plan's `items` (`--item`) name the items it may fill. Naming one is the agent's go-ahead for jev to choose a latch suggestion for it.
  - jev chooses among complete bundles (item, map, press button) plus `none`, even when there is one. It never builds a map, a field name or a ref, and it sees no value.
  - A login bundle, or one without a secret field, runs this section's fill: gate, pause, checks, match, set, submit, cleanup, and a second stage. An identifier-first sign-in is one operation.
  - A bundle that releases a card or identity secret stops `confirm_needed`, as above; `--confirm C` accepts that mapping and the release together, once.
  - `none`, or no complete bundle, stops `needs_fields`; a fitting item the plan doesn't name stops `needs_login` on a sign-in page (§6.4).
  - A code stage stops `needs_fields` before any decision, whether or not an item fits (§6.4 stop 1). For a TOTP stage the agent sends a literal `fill --item vN --field otp=eN --press eN`.
  - A passkey stage stops the same way, unless an item the plan names fits and holds a password: then jev may click through to the password path (§6.4 stop 1).
  - The trail records the item's title and the mapping, never a value. The result is `submitted`; whether the step's goal holds is decided by the step's own check.
- **A literal `fill` step** (§6.1) runs this section's fill unchanged, with confirm-before-release. It is a literal step like any other under B9's step policy, and a confirmation never makes a forbidden literal fill allowed.
- **The plan stops with `needs_owner`** on a CAPTCHA, and with `needs_fields` on a code stage, or on a passkey stage when no named fitting item holds a password (§6.4 stop 1). For a texted code or a passkey, the agent runs `handoff`, then sends a new plan.
- **No observation or screenshot is taken** from the start of a fill to confirmed cleanup. The trail records a suppressed-secret-phase marker instead.

**`handoff`.**
- `handoff` belongs to one session. That session is paused, and its window alone comes to the front, so the owner can act in the real browser window.
- Other sessions keep working in the background once no latch-filled secret is on any page (the pause rule above).
- The host's dashboard shows the pending handoff with its session hash and caller. The owner ends it with `latch browser handoff done`. Default timeout: 600 s.
- That session's refs from before the handoff are invalid afterwards.
- **On a host with no screen,** `handoff` returns `needs_owner` with `"reason":"no_screen"` at once, and so do login walls that need a person.

## 8. Screenshots and displays

- Screenshots to the agent are required on every host. They come from the engine, with the screen-to-page coordinate mapping, and are refused while a session is paused.
- **Hosts run engines headed by default:** headless got blocked on far more sites in the browser-poc site check (9/26 against 24/26 headed). Test suites, and a test host started with `latch serve --headless-browsers`, run headless so test windows never cover the owner's screen; checks that need a real window run only in an opt-in headed test command.
- On macOS the browser is a real window in the owner's session. It is visible but **never steals focus** (chef, 2026-10-19).
  - No startup window; every window and tab opens in the background; no tab activation.
  - **Only `handoff` brings a window to the front.** A plan stop, a dialog, a download or a popup never does.
  - Anti-throttling switches keep background windows rendering normally.
  - This is a conformance test on each headed host (§10).
- **On a Linux host,** each headed profile runtime's worker starts **one private Xvfb display**, inside the runtime's bubblewrap.
  - The runtime's sessions are windows on it.
  - The display lives as long as the runtime: closing one session never tears it down while another session still uses the runtime.
  - Each profile runtime has its own display.
  - **A Linux host never uses its desktop,** even with a monitor: the engine never gets the host's `DISPLAY`, so a Linux host always answers `handoff` with `no_screen` (sous, 2026-10-20).

## 9. The vault

- **One vault per latch install,** shared by every engine and profile, on the host, behind a **backend interface**. A backend can:
  - list item metadata;
  - add, update and remove items;
  - release exactly one value, into the §7 fill path only;
  - call audit hooks.
- **Always latch's job, whatever the backend:** the origin rules, the fill step, deciding a release, and value-free audit. A release needs only a matching origin; items are not bound to browser profiles (chef, 2026-10-19), and access rules belong to the later gatekeeper. There are no owner approvals for now (chef, 2026-10-19): a generic approval flow comes later, plugging into the gate; until then confirmations go back to the agent. Start with test and low-stakes accounts.
- **POC backend: latch's own store,** one file in the host's state directory, written only by `latch serve`, atomically.
  - Items are encrypted (AES-256-GCM, RustCrypto `aes-gcm`) with a random **data key**; the data key is locked by a **custody key**. In the POC the custody key is a 0600 file in the state directory on every OS (not the login Keychain: latch's `exec` sandbox can read login-Keychain items). The signed-app chunk (B4b) adds a Keychain half on macOS.
  - `exec` and file workers can read neither file; a conformance test checks it from inside both. On Linux the browser worker can't either (bubblewrap). **On macOS the browser engine runs outside latch's Seatbelt as the same user (§4), so a compromised engine main process could read both files: accepted risk with the chef's macOS ruling; a separate macOS user or B4b's Keychain half narrows it later.**
  - KeePass, Bitwarden, 1Password and Keychain items come later, behind the same interface.
- **Where each kind may fill** (all require HTTPS, non-opaque origins for both the field frame and the top-level page, and a page address; `http`, IP and local hosts are refused):
  - **login:** both origins share the item's registrable domain (§7 step 4); a login item must have a site.
  - **card, identity:** without a site list, any top-level site (parity with the Latch app); with an optional "only on these sites" list, the top-level page must share one of the listed registrable domains. The field frame may be another site (payment-processor iframes) but is checked, audited, and shown in the fill's confirmation (§7). There is no built-in list of payment providers (chef, 2026-10-20).
  - **note:** any top-level site, filled only into non-secret-marked fields the mapping names.
  - Every card and identity fill's top-level and field-frame sites are audited.
- **Item kinds, for parity with the Latch app:** login (username, password, optional TOTP seed — marked, since it holds both factors), card, identity, secure note. Field names follow rbw's Bitwarden-style model. Every item has a title, its site (required for logins; an optional site list for cards and identities) and a **revision** (an edit against an old revision is refused). Which fields are secret follows the Latch app's classifier, ported with its truth table as tests. TOTP via `totp-rs`, checked against the RFC 6238 vectors.
- **Commands** (owner-only):
  - `latch vault add --kind login|card|identity|note …`;
  - **`edit ITEM --revision N`**: change the title, sites, non-secret fields, or (prompted) secret fields of an existing item;
  - `list` (metadata only);
  - `remove`;
  - `import` (1Password 1PUX/CSV, Apple Passwords CSV, Bitwarden JSON/CSV, Chrome CSV; a picker, nothing selected by default; logins first, other kinds where the export carries them), porting the Latch app's import rules and tests.

  Secrets are prompted without echo, never taken from argv or the environment. An edit or removal against an older revision is refused (`stale_revision`). They go through `latch serve` over latch's own connection as an authenticated owner device, on the same machine too: no direct file edits. The secure link is a prototype beside v0 (§16).
- **Reuse rule:** the Latch app's rules, formats and tests are ported to Rust; no Latch code is imported and no sidecar is added. Libraries: `url`, `psl`, `aes-gcm`, `totp-rs`, `csv`, `zip`.
- **The agent never sees a value.** No command returns a stored secret.
- **No browser password stores or autofill.** The agent never uses them. Signing in by hand through a handoff, or an already logged-in profile, is fine.
- **Accepted risk (chef's ruling):** an agent script that ran on the page before a fill can capture the value. It is the same class as the old Latch app, and the gatekeeper deals with it later. Every fill's audit line flags it.

## 10. Engines and the driver contract

| Operation | The driver must |
|---|---|
| launch · close | Start the pinned build with its launch recipe over a private pipe, on its profile, with no listening port. Lock the profile. Reap every child. |
| tabs and events | Report targets with their window and opener, navigations, replaced documents, downloads (initiating frame and guid) and dialogs (with their target), so the core can assign each to its session |
| frames | Report every frame, cross-site included, with a document id and its effective origin, or "opaque". |
| run latch's script | Run the shared page reader in an isolated world; run agent `eval` in the main world when latch passes it on. |
| resolve | Turn a ref into a live element in its own document. Report visible, enabled and not covered. Map coordinates through frames. |
| input | Browser-generated ("trusted"), human-paced mouse, keys and text. Never `element.click()` from script. |
| wait · screenshot | Named conditions with deadlines; pixels plus the coordinate mapping. |
| files · downloads · dialogs | Set upload files; stage downloads under their guid and report them as handles; answer a dialog only through its own target |
| prepare fill · checked fill | Report, for each element a fill could target, its frame and effective origin, input type, `autocomplete` tokens, form and that form's submit buttons, so the core can suggest and check mappings; re-check element, document and origin and write in one step, or refuse. Never retry. |
| pause · resume | Freeze all agent access to every session of the runtime, and report a pause epoch |
| focus | Open windows, tabs and popups in the background; never activate the app except for `handoff` |
| capabilities | Report optional features. Anything missing returns `unsupported_capability` before dispatch, never a substitute. |

**Engines:**
- **CfT first:** Rust CDP driver; pinned CfT per OS and architecture; no `Runtime.enable`; `--disable-blink-features=AutomationControlled` in the recipe.
- **Camoufox second:** the Node helper, with pinned Node, Playwright (below 1.63) and Camoufox, `os` pinned to the host OS. An opt-in install (about 2.5 GB). The bar for dropping it is very high.

**Conformance suite.** latch accepts a driver only after it passes local test pages for: frames of every kind with the right origins; isolated globals with DOM changes visible; trusted input; checked fill refusing a wrong site, an opaque frame and a replaced document; the pause holding (screenshots included); dialogs, pop-ups and files; and, from inside `exec`, no reachable port and an unreadable vault key. An engine that fails only the fill tests is accepted without `fill`. Also required:

- two sessions on one runtime keep their tabs, downloads (same file name at once) and dialogs apart, while sharing cookies;
- a pause holds across both sessions;
- closing one session leaves the other's browser (and, on Linux, its display) working;
- **headed focus:** with another app frontmost, nothing activates the browser: launch, a new session window, a new tab, a popup, a download, a dialog, a page's modal passkey prompt (B5d). Only `handoff` does.

## 11. Audit

The host's hash-chained NDJSON audit gets one record per browser request and one per dispatched action:
- caller (client key), session (id hash), task label, profile and engine;
- operation, plan request id, step and action number;
- target, how it was chosen (`ref` or `model`), and each head's p;
- risk p, and the confirmation id when one was used;
- origin and path (no query);
- check kind and the step's outcome (`checked`, `model_assessed`, `already_held`, `dispatched`, stop code);
- dispatch state (`sent`, `refused`, `unknown`);
- `shared_site`, reservation and `step_not_allowed` events;
- the turn id or parent process id.

**Never written:** page text, field values, typed text (`type` records length and sha256), screenshots, vault values, codes, seeds, or session ids (only their hash). Page lines and screenshots live only in the session's trail bundle (§6.7).

Additions:
- **Fills:** item and revision; the mapping (item field → ref, never a value), and whether it came from a suggestion (its id, and for a plan bundle jev's p) or from the agent; the number of stages; the top-level origin and each target frame's origin; each field's outcome and `hint_mismatch`; and `agent_code_before_fill`.
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
- A `latch browser` agent guide ships in the repo, and `latch browser --help` points to it.
  - Its examples lead with the plan command and typed checks.
  - It explains each stop code and the move that follows it.
  - It keeps `act` for confirmations and repairs.

## 13. Testing (chef, 2026-10-19)

latch is primarily a consumer agent's browser, so testing answers two questions: can an agent log in and get things done, and which consumer sites block which engine. Source research: kitchen notes `research-browser-benchmarks.md`. No heavy Docker suites (WebArena and similar) for now.

- **Basics (adopted fixtures, served locally):** a handful of MiniWoB++ tasks (MIT) and Stagehand's interaction fixtures (MIT) — e.g. `choose-list`, `iframe_form_filling`, `multi_tab` — scored by their own pass checks, run through `latch browser`.
- **Real public sites, read-only:** a few WebVoyager tasks (Apache-2.0) such as arXiv help, GitHub latest release, a recipe with constraints; answers checked against the page at run time.
- **Security, where simple:** local pages adapted from WASP/AgentDojo (an injected post trying to leak a made-up canary to another local origin), SCAM (a lookalike login: no vault release, nothing reaches the "attacker" origin) and ST-WebAgentBench ("draft, don't send": zero sends).
- **latch's own promises:** a vault login with a handoff; the pause holding through a fill and its cleanup.
- **Fill fixtures (local pages; B5d).** They cover what real pages showed in the fill research (`design-fill-fillr.md` §3):
  - identifier-first sign-in, with the second stage as a new document and as new controls in the same document;
  - one-page logins with tokens, and without (`type=password` only);
  - a wrong token (a "phone or email" box marked `tel-national`), and a revealed password (text with `current-password`);
  - a registration form (`new-password`; two password fields);
  - hidden `aria-hidden` duplicate inputs carrying card tokens, as Shopify's checkout has;
  - a discount box beside card fields, and an unhinted text card field;
  - card fields in a cross-site processor frame;
  - a second stage asking for a code, a method chooser, and passkey and SSO buttons;
  - a passkey-only page (a `webauthn` field, no password or `username` field); a passkey-first page with a link to its password path; and an identifier field marked `username webauthn`;
  - two legitimate forms with repeated tokens on one page;
  - a lone `email` newsletter box on a page a login item fits.
- **Consumer-site matrix (the bake-off's core):** for each engine (CfT; Camoufox once B8 lands), each site: does the home page load, can the agent search/browse logged out, does the login page load, and is the agent challenged or blocked (and where). Starting list: DoorDash, Costco, Redfin, plus a dozen common consumer sites (shopping, food delivery, travel, real estate, banking login pages). Logged-out only, except sites where the chef has added his own login to the vault and asked for it. Result: a table of site × engine → works / challenged / blocked, with screenshots, repeated on at least two days. This measures blocking; it does not build evasion.
- **Plan suite (local fixtures; development data, §6.3).** A fictional shop and forms, scored by independent, identity-sensitive checkers (the right item, the right story, not just the right page type). It covers:
  - navigation and search;
  - a relational target ("the cheapest");
  - a dropdown already on the right option;
  - a cookie modal;
  - a same-URL multi-stage wizard;
  - an out-of-stock item;
  - a misleading success banner with the wrong item;
  - needed text that wasn't supplied;
  - **one note field with one unrelated supplied value**, such as a street address given as a fact;
  - a value containing ` -> `;
  - a sign-in wall in the middle of a step (identifier-first);
  - a card form: reached by a plan that names no card (`needs_fields`), and by one that names it (jev chooses the suggestion; `confirm_needed` shows the mapping);
  - checkout with "Place order" (confirmed, and repriced before confirmation);
  - Enter in a form that submits an order;
  - a delete-account button;
  - a CAPTCHA stand-in.
- **Agent adoption test:** B9, on held-out cases.

## 14. Open questions

1. **Nested sandboxes.** macOS: answered — they can't nest; engines run outside latch's Seatbelt with their own sandbox on (§4, chef 2026-10-19). Linux: B7 proves bubblewrap nesting. Chrome's sandbox is never disabled; if Linux can't nest, stop and go back to the chef.
2. **Real login forms.** Does the one-step value set work on framework-built ones? Public practice forms are tested before "no typing" is frozen.
3. **CfT on the Pi.** If pinned `linux-arm64` CfT fails there, the fallback is Debian Chromium on the same driver, as a separate engine package.
4. **Approvals.** Answered for now: none; a generic approval flow comes later (not Plow's payment-approval flow), and confirmations go back to the agent.
5. **Which decision model, and its privacy terms,** before real accounts. **And how hosts reach it without a key: via Plow inference (chef's lean, 2026-10-19), which needs a Plow API route — B5b.**
6. **Calibration.**
   - The starting values: status ≥ 0.7 with an evidence line, target ≥ 0.5, risk ≥ 0.5; 8 actions per step, 30 per plan, 120 s; the 7-day trail.
   - They are frozen for each evaluation.
   - A change informed by evaluation results turns those cases into development data, and the next evaluation needs a fresh, disjoint held-out set (§6.3).
7. **Block rates.** The headed engines' block rates come from one IP in one run. Re-measure with complete drivers.
8. **What the logs show.** Do the `eval` workaround tags and `unsupported_capability` hits justify raw commands, or new ordinary commands?
9. **The plan command's name.** Pending the chef, who asked "why not `steps`?". A rename touches only its §3 entry and the §5 table.
10. **The trail's retention and size.** 7 days and per-action screenshots are trial values.
11. **Modal passkey prompts.** A site calling `navigator.credentials.get()` without conditional mediation raises a browser or OS sheet. Whether that brings a headed window forward on macOS is unverified; the headed focus check (§10) answers it. If it does, the fix needs the chef. latch installs no virtual authenticator by default.
12. **Suggestion coverage.** How often real login and checkout pages get a complete suggestion from standard hints alone. B9's site matrix measures it on login pages; the answer decides whether suggestions are convenient enough, never whether to add a keyword classifier.

## 15. Later

- **Coordinate clicks (`act click --at X,Y`).** They come back only with a binding equal to a ref's:
  - at decision time, latch hit-tests the point and records the element there, its document and its reviewed context;
  - it re-checks that the same element is still under the point at dispatch, and refuses with `stale_snapshot` if not;
  - the risk decision and confirmation work as for that element (§6.5);
  - they obey the step policy, if B9 ships one.
- **Runner-issued task identity.** A task identity the agent runner issues and the gate verifies, so ownership no longer rests on one client key plus a session id held by convention.
- **Cross-host site reservations.** An owner-wide reservation record through a Plow API route, so tasks on two serving hosts can't both take the same site. Until then, `--reserve-site` is host-local.

## 16. Phone link (prototype, chef 2026-10-19)

A prototype beside v0, not part of the chunks: the owner enters a secret on their phone and the agent never sees it.

- `latch vault link --kind K --title T --site S [--field L=V…] [--ask L…] --page URL` is owner-device only. It prints a one-time link and a QR code. Secrets on the command line are refused.
- The link opens a small static page (the shim). The shim runs iroh in the browser and dials the host through the relay; everything that matters sits after the `#`, which the shim's server never sees. latch serves the form itself over that connection on a separate phone protocol that can only fetch its link's form and save its link's item.
- A link is 128 bits, kept on the host only as a hash, in memory, used once, expires in 15 minutes, and only adds a new item. Every secret and a note's text are required; an empty save is refused and the link stays open.
- The save goes through the same path as `vault add` and is audited as `vault_link` without values. The agent sees only the new item's handle, as for any vault item.
- Trust that remains: whoever serves the shim. The prototype serves it locally; a public HTTPS home for away-from-home use is the chef's call.
- Evidence: `just phone-e2e HOST` (emulated iPhone, login, identity and note saves, reuse refused, agent sign-in, leak search) and a real Mobile Safari run in the iOS Simulator.

## Chunks

### Global constraints

- **Product stage:** proof of concept on real hosts, not throwaway (header of this spec). Reviewers size the bar to that: clean, testable code that later work builds on; no hardening §2 defers.
- **Builds on latch-cli as it is** on branch `latch-cli` (spec `2026-10-14-latch-cli-poc-design.md`): same client/host split, iroh transport, gate, hash-chained audit, exit codes, state directory, plugin layout and `install.sh`. Nothing in §2's non-goals gets built.
- **One kind of session; no raw commands; no browser password stores or autofill; product hosts headed by default (headless only for tests and test hosts); private pipes only — never a listening debug port.** These hold in every chunk.
- **No secret** (vault value, TOTP seed or code, decision-model key) ever appears in a result, error, log line, audit record, test output or commit.
- **Tests and test data:** local test pages and public practice sites, made-up credentials. Exceptions (§13): logged-out visits to the §13 consumer sites for the block matrix and read-only public-site tasks; and real-account logins only on a site the chef has himself added to the vault and explicitly asked to test, read-only (no purchases, sends or account changes). Never the chef's real Chrome or Safari profile, real Keychain items, `~/.latch`, `.dev/latch-prod`, or the Pi's `~/latch-dev/state` except where a chunk needs a dev host there. The jev key is read from `/Users/plucas/.claude-kitchen/plow-latch-cli/secrets/jev-api-key` into the host's state dir for tests, never printed; spend under $2 per chunk.
- **Host:** the Mac dev host may be restarted for checks. The Pi has exactly one dev build in `~/latch-dev/`; `~/.local/bin/latch` and `~/.latch` stay empty.
- **Global installs** are avoided; any unavoidable one is appended as a new row to `/Users/plucas/.claude-kitchen/plow-latch-cli/notes/global-installs.md`.
- **NEVER git push or open a PR.** Commit locally on `latch-cli`, one or more commits per chunk.
- **Stop and report** (`NEEDS_CONTEXT`) rather than weaken a rule above to make a chunk pass. §14 open question 1 is the known one: on macOS it is settled (engines run outside latch's Seatbelt, §4); on Linux, if an engine's own sandbox can't run inside bubblewrap, stop for the chef. Never disable an engine's own sandbox.
- **Tests never use the chef's serving host `mac`, or its profiles.** Cooks use their own dev host (`mac-kitchen`), test profiles and test sessions.
- **Development and held-out cases are kept apart** (§6.3).
  - A chunk may set starting values on development cases.
  - Nothing is tuned against held-out cases.
  - Cases that informed a change are development data from then on.
- **No generative text model, and no keyword stand-in for the risk decision, in any chunk.** Every typed string comes from a plan's values or facts, or from the vault. **No keyword classifier of fields or buttons in fills** (§7).
- **Nothing activates a browser window except `handoff`.**

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
Landed with the separate `login`, `fill` and `fill-code` commands; B5d folds them into the one `fill` and proves this acceptance again through it.
Implements: §9 (backend interface, own store with data key + 0600 custody key, four item kinds, revisions, owner commands over latch's connection, import), §7's fill, §10 prepare/checked fill and pause, §11 fill records.
Interfaces: consumes B2 · produces the vault backend interface later backends implement, the checked write, pause and cleanup the one `fill` uses.
Done when: a login item added from a client logs in to a public practice login site via `fill`, including on a subdomain of its site; it is refused (`origin_mismatch`) on a lookalike domain, on `http`, and on a sandboxed-iframe test page; a card item fills a local checkout test page with `fill … --press` (number, expiry reshaped, CVV) and the card fields are gone before the agent resumes, while a secret-field fill without `--press` is refused; an identity item fills a local address form; `eval` is refused while a value is on the page; screenshots and observe return `session_paused` during fills; a TOTP item fills a local TOTP form (`--field otp=…`) and the code matches RFC 6238 vectors in unit tests; a 1Password 1PUX and a Chrome CSV fixture import through the picker; the ported classifier truth table passes; a grep of results, audit and logs finds no value; exec and file workers can't read the vault or its key; the audit flags `agent_code_before_fill` when eval ran first and records every card fill's site.

### Chunk B4b: signed latch on macOS, Keychain half, Latch app import
Implements: §9's macOS Keychain half of the custody key and import from the Latch app's vault. Blocked on the chef: an Apple App ID with keychain sharing, a provisioning profile, and where the Developer ID certificate lives.
Interfaces: consumes B4 · produces a signed, notarized `latch` in a minimal app bundle.
Done when: the bundled `latch` is signed and notarized; the custody key is split between the state directory and a data-protection Keychain item ("this device only"); a copy of the state directory on another account can't open the vault; a sandboxed `exec` worker can't obtain the Keychain half (including by running the signed `latch`); `latch vault import --from-latch-app` imports a throwaway Latch-app vault through the picker without writing to the app's files.

### Chunk B4c: vault edit
- **Implements:** §9's `latch vault edit`.
- **Interfaces:**
  - consumes B4's store (`update` at a revision), owner-device gate, prompts and classifier;
  - produces `edit` for owner devices.
- **Done when:**
  - **What can change:**
    - an owner device changes an item's title, sites and non-secret fields;
    - it replaces a login's password and authenticator key through no-echo prompts;
    - a secret can't be given on the command line or in the environment;
    - an edit against an older revision returns `stale_revision`, and the item's kind can't change;
  - **The new value is used:** a `fill` of the login after the edit fills the new value on the practice site;
  - **Agents can't edit:** an agent peer gets exit 4;
  - **The audit** records the item, the names of the changed fields and the new revision, and no value appears in any result, audit record or log.

### Chunk B5: the shared step contract (reworks the step-list B5 at `7eae741ff`)
- **Implements:**
  - §5's narrowed `act`, the removed forms, and the results and exit table;
  - §6.1 literal steps (including `fill`) and checks;
  - §6.5 confirmations (clicks, Enter, secret fills; `model_unavailable`);
  - §6.6 model interface;
  - §6.7 history and journal;
  - §7's confirm-before-release for the standalone `fill`;
  - §11's new records.
- **Interfaces:**
  - consumes B2 and B4, and B5's existing provider interface, answer validation, risk question, request ids, typed checks and dispatch at `7eae741ff`;
  - produces the literal-step executor path, typed checks, confirmations, the durable journal and the result shape that B5c, B5d and B2b build on.
- **Done when:**
  - **Literal steps take effect:**
    - single `act` steps click, type and select with a real effect (`select` changes the option, shown by the field state);
    - each runs its check: a failing check returns `check_not_met`; no check returns `dispatched`, not `reached`.
  - **Typed checks match by component:**
    - `origin`, `path`, `query`, `text`, `field`, `download` and `new text` behave as §6.1 says;
    - `path: checkout` doesn't match `https://checkout.invalid/login` or `/login?next=checkout.html`.
  - **Confirmation is enforced:**
    - a literal click on "Place order" stops `confirm_needed` with the reviewed context;
    - `do --confirm C` with the returned confirmation carries it out once, and a second use is refused;
    - after the fixture's total changes from $10 to $100 (same ref, URL and button name), the confirmation is refused `confirm_stale`;
    - Enter in the checkout form's field stops `confirm_needed`;
    - a standalone `fill … --press` of a card stops `confirm_needed` before any value is released, and its confirmation fills, presses and cleans up.
  - **No model, no pass:**
    - with the decision model unreachable, a literal click and an Enter stop `model_unavailable` with nothing sent;
    - a confirmation issued before the outage still dispatches;
    - literal `type` and `select` still work.
  - **The journal survives a crash:**
    - a host killed mid-plan and restarted reports the in-flight action as `unknown` in the plan command's `--history R`;
    - a dropped reply is resolved by request id with no second dispatch.
  - **The removed forms are usage errors,** `act click --at` included.
  - **The audit** carries the new fields, and no page text or session id.

### Chunk B5b: decision model through Plow inference
Implements: §6 product path — the host calls the decision model through a Plow inference route with its own Plow credential; no model key on hosts. Blocked on a Plow API route (to add to the pending Plow API PR alongside the offline and take-over routes).
Interfaces: consumes B5's provider interface · produces a `plow` provider and removes the need for a host key file.
Done when:
- with no key file on the host, B5's and B5c's Done-when tasks pass using the `plow` provider against a dev Plow stack;
- with the Plow route made unreachable, a click stops `model_unavailable` with nothing sent;
- the host's audit records provider `plow` and the model;
- the key file path is used only when explicitly configured for development.

### Chunk B5c: plans and the decision loop
- **Implements:**
  - the plan command with the short form, `--plan-file`, `--dry-run`, `--output` and `--history R [--shot N --output F]`;
  - §6.1–§6.4 (operation contract with `fill` → `needs_fields`), and the trail bundle in §6.7;
  - §6.8 metric fields;
  - the §13 plan suite.
- **Interfaces:**
  - consumes B5 and the page reader;
  - produces instruction steps, the per-pass decision request, completion and stop rules, and per-action screenshot handles that clients on any machine can fetch.
- **Done when:** with the frozen starting values, on the Mac dev host, against the §13 plan suite (development cases), in two runs with jev:
  - every plan ends with its listed outcome;
  - every `reached` passes its identity-sensitive checker, with **zero false `reached`**;
  - the wrong-item banner never reaches;
  - **The reader shows what the model needs:**
    - with a cookie modal open, covered elements are not offered and the plan goes through;
    - a dropdown already on the requested option ends `already_held` with no action;
    - a flash message from an earlier step never makes a later step `already_held`;
  - **Repeats and limits behave:**
    - the same-URL wizard completes through repeated "Next";
    - a no-op click is never sent twice: the step stops `repeat`, `no_progress` or `stuck`, with at most one dispatch and no effect on the page;
    - a step without a typed check clicks a control once ("add one more Greek Salad" adds exactly one);
    - the out-of-stock step stops `blocked`, and the missing note stops `needs_text`;
  - **Text and fields are never guessed:**
    - with one note field and one unrelated supplied value (a street address fact), a step asking for a note that wasn't supplied stops `needs_text` and types nothing;
    - an instruction step reaching the card form stops `needs_fields` with the form's candidate refs;
  - **Values and URLs are literal:**
    - `add the note "A -> B"` types `A -> B`, shown in `--dry-run` and by the field;
    - `navigate` reaches only a quoted URL;
  - **Results are labelled honestly:**
    - a final step judged only by the model returns `review: true`;
    - **from another machine:** from a client on a different machine than the host (the Pi client against `mac-kitchen`, or the reverse):
      - a plan runs;
      - the plan command's `--output F` writes the final screenshot;
      - `--history R --shot N --output F` fetches an action's screenshot;
      - both arrive with sha256 matching the host's copy;
      - a grep of the plan's JSON result finds no host path (the host's state directory never appears);
    - with no model, instruction steps return `model_unavailable`;
  - **Measurement:** the report lists, per plan, actions, model calls, the model's total time and the cost, and keeps every run.

### Chunk B6: handoff and screenless hosts
Implements: §7 handoff (one session's window to the front, dashboard notice, `handoff done`, that session's refs invalidated, `no_screen` fast fail), and §8's per-runtime Xvfb display.
Interfaces: consumes B2, B4 · produces the owner handoff flow.
Done when: on the Mac a handoff pauses the agent, the dashboard shows it, `latch browser handoff done` resumes it and old refs are refused; with the display check forced to "no screen" in a host test, the call returns `needs_owner` with `no_screen` at once (the real screenless check is B7's).

### Chunk B5d: one fill, suggestions, and fills inside plans
- **Implements:**
  - §7: the one `fill` (removing `login` and `fill-code`); `observe`'s items, `page_offers`, suggestions and alternatives; the mapping checks and `bad_target`; a login's second stage; per-field results; confirm-before-release with the mapping and frame sites;
  - removing the reader's English button words and keyword field patterns: a press button is the form's single submit button or the one the agent names;
  - §5's command and exit rows, §6.1's literal `fill` and `items`, §6.2's `fill` operation with bundles, §6.4 stops 8, 9 and 11, §6.5's fill confirmation;
  - §7 inside plans: suppressed secret phases, and `needs_owner` → `handoff` → new plan;
  - §10's prepare-fill report and the passkey-prompt focus check; §11's fill records; §13's fill fixtures;
  - popups that a step's click opens.
- **Interfaces:**
  - consumes B4's vault, checked write, pause and cleanup; B6's handoff; B2b's browser-wide pause; B5 and B5c;
  - produces the one `fill` for agents and plans, which B8's fill conformance and B9's guide use.
- **Done when** (on the §13 fill fixtures unless a practice site is named; plans through jev on the dev Plow stack):
  - **One command:**
    - `login` and `fill-code` are usage errors that name `fill`;
    - B4's login, card, identity, TOTP, origin and leak rows pass again through `fill`, on the same practice sites and local pages;
    - `--field otp=…` fills split code boxes in order.
  - **`observe` tells, without values:**
    - fitting items list their fields as present, secret or derived; a grep of the result finds no value and no username;
    - `page_offers` is right on the identifier stage, the code stage and the passkey-only page, and the passkey and SSO buttons come back as alternatives; the `username webauthn` field is an identifier stage, and a plan step signs in through it;
    - suggestions come from tokens, and from `type=password` plus the field before it on the tokenless login;
    - no suggestion is made on the registration forms, for a code field, or for the lone `email` newsletter box; two submit buttons give no press;
    - the hidden duplicate inputs are never suggested and are refused as targets;
    - the two forms with repeated tokens each get their own suggestion.
  - **`--suggested S`:**
    - it fills exactly S's mapping;
    - after the fixture moves a field, or the item's revision changes, it is refused `stale_snapshot` with a fresh suggestion, and nothing is released.
  - **The checks refuse what the markup contradicts** (`bad_target`, field and reason named; the audit shows no release):
    - refused: a password into a text field without `current-password` and into `new-password`; a username into a password field; `otp` into a `current-password` field; a card number into a textarea, a search field and an `email`-token field; two item fields into one element;
    - allowed: a revealed password; an unhinted text card field on an explicit map, whose confirmation shows that map; an email into the `tel-national` box, flagged `hint_mismatch`.
  - **A login's second stage:**
    - on both identifier-first fixtures, one `fill` of the username and its button, explicit or suggested, signs in through the password stage inside one pause, reporting two stages;
    - a second stage asking for a code, a method chooser, or two submit buttons ends after cleanup with `needs_fields`, carrying the new stage's candidates, offers and alternatives, with no password released;
    - a second stage on another site is refused `origin_mismatch`, with nothing released.
  - **Card confirmation:**
    - a standalone card `fill` stops `confirm_needed` before any release, showing the exact mapping, the processor frame's site and the press button;
    - its confirmation fills, presses and cleans up once, and a second use is refused;
    - after the fixture swaps the processor frame, or a mapped element, the confirmation is refused `confirm_stale`.
  - **Inside plans:**
    - `sign in` with `--item vN` passes on an identifier-first fixture as one `fill` operation, and on a one-page practice login;
    - a sign-in wall met in the middle of `proceed to checkout` is signed through when the plan names the item, and the plan continues to checkout; without the item named, it stops `needs_login` listing the fitting item;
    - a TOTP stage stops `needs_fields` naming the code field, and a following plan's literal `fill --item vN --field otp=eN --press eN` passes;
    - with no vault item fitting the site, a code stage and the passkey-only page each stop `needs_fields` before any decision, with an empty item list, `code` or `passkey` in the page's offers, and (for the passkey page) its button as an alternative; nothing is typed or clicked;
    - on the passkey-first page, `sign in` with a named login that holds a password clicks through to the password path and signs in; with the item not named, or naming one without a password, it stops `needs_fields` before any decision;
    - a plan that names the card stops `confirm_needed` with jev's chosen mapping, and its confirmation fills, presses and cleans up; a plan that names no card stops `needs_fields`;
    - jev is offered each bundle with `none`, and a grep of the decision requests finds no value.
  - **Owner handoff:** a CAPTCHA stand-in stops `needs_owner`; `handoff` brings only that session's window forward; a new plan continues afterwards.
  - **No secret reaches the agent:**
    - no observation or screenshot is taken during a fill, and the trail shows the suppressed-phase marker;
    - a grep of results, trail, decision requests, audit and logs finds no value.
  - **No word lists:** a grep finds no button-word or field-keyword pattern in the reader's fill code, and B5c's plan suite still ends with its listed outcomes in one run.
  - **Focus:** the opt-in headed focus check (§10) includes a page raising a modal passkey prompt, and its result answers §14 question 11.
  - **Popups:** a popup opened by a step's click is followed, belongs to the session, and opens in the background.

### Chunk B2b: task sessions on a shared profile
- **Implements:**
  - §3's runtime and sessions, and §4's per-profile worker (Linux scratch included);
  - §5.1–§5.3;
  - §7's browser-wide pause and per-session handoff;
  - §8's focus rule and the per-runtime display;
  - §10's ownership, focus and conformance rows;
  - the session fields in §11.
- **Interfaces:**
  - consumes B2, B4, B6 and the landed session guard (`f092f67aa`..`a9714d692`);
  - produces profile runtimes, capability session ids, download and dialog attribution, `shared_site` and `--reserve-site`.
- **Done when:**
  - **Sessions are separate:**
    - two clients with different keys each `open` the same profile and get distinct sessions and windows in one runtime;
    - `profile_busy` no longer occurs;
    - a cookie set in one is seen in the other;
    - each sees only its own tabs, popups, downloads (two at once with the same file name, assigned by frame and guid) and dialogs;
    - closing one session leaves the other working;
  - **The id is the capability:**
    - every command on another key's session, even with the right id, returns `not_owner`;
    - **two tasks under the same client key** each `open` a session;
    - task B, without A's id, can't act on, observe or close A's session (`no_such_session`);
    - `sessions` shows neither id;
    - the audit holds only id hashes;
  - **Plans don't block each other:** two plans run at the same time on different sites without waiting on each other;
  - **The pause:**
    - a fill in one session returns `session_paused` to every session until cleanup is confirmed;
    - a plan elsewhere waits, then observes again;
    - a handoff fronts only its own window, and the others go on after cleanup;
  - **The same site:**
    - `shared_site` appears when both sessions act on the same site;
    - with `--reserve-site`, the other session's action there gets `site_busy` before anything is sent, through a stop and a confirmation, until `close`;
  - **Headed focus:** with another app frontmost, nothing but `handoff` activates the browser;
  - **The audit** carries caller, session hash and task label.

### Chunk B7: Linux and the Pi
Implements: §8's one Xvfb display per profile runtime; §12 Linux install (CfT linux-arm64, Xvfb); the bubblewrap browser worker per profile runtime. Answers §14 questions 1 and 3 for Linux.
Interfaces: consumes B1–B6 and B2b · produces a working Linux/Pi browser host.
Done when: on the Pi dev host (`~/latch-dev/`), CfT runs headed on a private Xvfb display inside bubblewrap with Chrome's sandbox on, and never on the host's own `DISPLAY`, even when the serving shell has one; a client on the Mac opens a public site, observes, acts, and receives a screenshot from the Pi; a vault login works there with the 0600 key; `handoff` returns `needs_owner` with `no_screen` at once; the exec-side port and key checks pass. On the Pi, two sessions on one profile share one runtime and one display; closing one leaves the other's browser and display working; a session on a second profile gets its own runtime and display. If CfT linux-arm64 fails, stop and report before switching to Debian Chromium.

### Chunk B8: Camoufox as the second engine
Implements: §10 Camoufox engine (Node helper, pinned runtime, JSON-lines driver ops), `install.sh --with-camoufox`; the conformance suite as a reusable runner for any driver.
Interfaces: consumes the driver contract from B1–B6, B2b and B5c · produces the second engine and the conformance runner the bake-off uses.
Done when: the conformance runner passes every test for CfT, and every non-fill test for Camoufox on macOS (if Camoufox fails only fill tests, `fill` is disabled for it and the failures are listed; any other failure means not done); the B2, B5 and B5c Done-when tasks pass unchanged with `--profile` bound to a Camoufox profile, the headed focus test included.

### Chunk B9: agent guide, agent adoption test, and the §13 set
- **Implements:**
  - §12's agent guide and `latch browser --help`;
  - §6.8 metric reporting;
  - §13;
  - the step policy, which this chunk adds for its arms, and the decision whether one ships (§6.1).
- **Interfaces:**
  - consumes B1–B8, B5c, B5d and B2b;
  - produces the guide, and the shipped step policy.
- **Done when (a), the agent adoption test:**
  - **Held-out suite:** 16 fixed tasks, disjoint from B5c's plan suite in tasks, page layouts and sites:
    - 12 on new local fixtures, at least 3 of them prelabelled as needing a repair, plus traps (wrong item, repriced confirmation);
    - 4 read-only public tasks;
    - each scored by an independent, identity-sensitive checker.
  - **The step policy (added here).** A host setting that decides which literal steps a session may run: `any` (refs from any page latch returned to the session), `stop` (only refs returned in that session's latest stop) or `none`. It is enforced in the shared step dispatcher, whether a step came from `act`, the standalone `fill`, the plan command's short form or `--plan-file`; a forbidden step is refused with `step_not_allowed` (exit 4) before anything is sent; `--confirm C` is never forbidden, and creating a confirmation never makes a forbidden literal step, `fill` included, allowed. A separate switch turns instruction steps off, for arm A.
  - **Arms,** set by the host's step policy and enforced in the dispatcher for every route:
    - **A:** instruction steps off; literal steps on refs from any page; `eval` (the baseline);
    - **B:** plans and confirmations; literal steps `none`;
    - **C:** plans and confirmations; literal steps `stop`;
    - **D:** plans and confirmations; literal steps `any`.

    `eval` is available in every arm.
  - **Runs:**
    - two caller integrations (Claude Code and Codex), each a fresh agent given only the guide;
    - two runs per task, arm and caller;
    - turns grouped by turn id.
  - **The report** keeps every attempt, and gives per task and arm:
    - success;
    - caller invocations and turns;
    - plan share;
    - confirmations and how they were answered;
    - repairs, and `step_not_allowed` refusals;
    - `eval` escapes;
    - stops by code;
    - wrong targets;
    - model time and cost.
  - **Common gates, for B, C and D:**
    - **success:** no more than 5 points below A;
    - **zero:** false `reached`, confirmation violations, and actions repeated after an `unknown`;
    - **at least 85%** of ordinary actions inside plans;
    - **at most 10%** `eval` escapes;
    - **at least 30%** fewer caller invocations than A on multi-action tasks.
  - **Repair gate, for C and D only:** at least 80% of the prelabelled repair cases solved through literal repair steps.
  - **Decision:**
    - **Plan-only.** If B passes the common gates, and neither C nor D solves more of the prelabelled repair cases than B, ship policy `none`: literal repair is dropped, confirmations stay.
    - **Repair.** If C or D passes both the common gates and the repair gate, and solves more repair cases than B, ship the narrower passing policy: `stop` before `any`.
    - **Bypass.** An arm that falls below the plan-share gate (agents collapsing into literal steps) is not shipped.
    - **No arm.** If no arm passes the common gates, jev is no longer the primary executor.
  - **A result that changes wording, thresholds or budgets** makes this suite development data. A later evaluation claim needs a fresh held-out suite (§6.3).
- **Done when (b), the §13 set:**
  - it runs from one command against a host and engine, and writes a report (task → pass/fail with evidence; security cases → outcome);
  - the consumer-site matrix is filled for CfT and Camoufox on the Mac (two runs, on different days), with screenshots, plus, for each site, a logged-out plan (a search through the plan command) with its outcome, and for each login page whether `observe` offers a complete suggestion (read-only: nothing typed or submitted).
