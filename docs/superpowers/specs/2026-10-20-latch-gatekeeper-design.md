# Latch gatekeeper on hosts — design

2026-10-20. Status: draft for the head chef's review. Product stage: **internal tool, early users**. Size the bar to that: simple, understandable, permissive at first, hardened as we learn.

Sources: `gatekeeper-v0.md` (the judge and its measured results), `gatekeeper-plan.md` (the converged expert plan), `client-creds-*.md` (agent identity), all in the kitchen notes. Checked against latch-cli `99b2c0b` and Plow `origin/main` with `POST /v1/decisions` (#2425).

## 1. Purpose

People run `latch serve` on their Mac or a cloud VM. Agents reach that host from elsewhere with `latch … --host NAME`. Today any live agent on the owner's account can use exec, files, the browser and Google on every host. This spec puts **one gate** in front of every agent request: the gatekeeper. It allows what the owner actually asked for, in their own chat with that agent, and nothing else.

## 2. Settled decisions

1. **The gatekeeper is the gate.** No per-agent permission lists. Intent comes from the owner's messages in Plow chat, never from what the agent says.
2. **v0 judge, unchanged.** One question to jev through `POST /v1/decisions`: is the action COVERED, NOT_COVERED or FORBIDDEN by what the owner said? Allow at P(COVERED) ≥ 0.5. Model jev-1.14. Held-out results: 12/164 false allows, 4/52 false blocks.
3. **No dollar limits** of any kind. An amount the owner states in chat is part of what they asked for.
4. **Deny → ask → retry.** A refusal carries a reason. The agent asks the owner in chat, and the owner's reply is just the next message the gatekeeper reads.
5. **Identity comes from the credential.** A client next to a Plow agent registers with that agent's own credential; Plow records `agent_uid`; the host reads it from `GET /v1/latch/peers/{key}`. Names are labels and are never trusted.
6. **No Plow change.** Everything uses routes already on Plow `main`.
7. **The owner's own machines are not judged.** A device login is the owner acting.

## 3. Where it runs

On the host, inside `latch serve`. The host already checks every caller with Plow on every operation, holds the operation and its arguments, and writes the audit.

For an agent's request, the host reads that agent's chat with the owner through existing routes, using its own login:

| Step | Route |
|---|---|
| agent → its line | `GET /v1/agents/{agent_uid}` |
| line → chats | `GET /v1/chats` |
| chat → messages | `GET /v1/chats/{uid}/messages`, paginated back to the start of the chat or until the state would exceed the 64 KiB `/v1/decisions` limit |

- **Which chat.** If the agent's line has one chat, that one. Otherwise the client names it (`--chat` or `$LATCH_CHAT`), and it must be on the agent's line; with none, the call is refused `which_chat`.
- **Who is the owner.** A message counts when Plow classifies its sender as the account owner. Members, the agent and Plow are context, never permission. Plow's message routes don't expose how a message arrived, so owner-looking messages written through Plow's test-injection route (`POST /v1/chats/{uid}/inbound`, which needs a device or test credential) can't be told apart in v0. That is a named limit (§10), fixed later with proper provenance, which needs a Plow change.
- **Truncation is named.** If older messages don't fit, the judge is told so. A missing older "never do X" can cause a wrong allow; that is a stated limit, not a claim of safety.
- **Already done.** The judge also sees this agent's operations dispatched since `serve` started, with outcomes (newest 20), labeled as this run only.
- **Every judgment reads the chat fresh,** so an owner's "stop" always counts. There is no reuse cache.
- **Where the chat goes.** The judge's input (the chat window and the action, never vault values) goes to jev through Plow's `/v1/decisions`, which routes to OpenRouter with zero data retention. That is the product's one outside data flow, and the owner's own credits pay for it.

## 4. What is judged

| Area | Judged | What the judge sees | Not judged |
|---|---|---|---|
| **exec** | every call | argv and working folder | — |
| **files** | every get and put | path; for put, size and file name | — |
| **Google** | every call | the command and its arguments | — |
| **Browser: sites** | opening a site this session hasn't been judged for (`open`, or `tab` with a URL) | the URL | — |
| **Browser: plans** | each `do` plan, once | the plan text as written, the current URL and title, every host it names, any vault items it may fill (kind and title) | the plan's individual steps, clicks, typing |
| **Browser: vault use** | every `fill`, and every confirmation that releases a card or identity | item kind and title, field names, the site. Never values. | — |
| **Browser: code and files** | every `eval`, upload and download | the script, or the file's name and size | — |
| **Browser: the rest** | — | — | clicks, typing, `act`, page reads, screenshots, `--confirm` of an ordinary click |
| **Vault edits** | not reachable by agents (owner-only, as today) | — | — |

Unjudged browser actions still pass the browser's own checks: the per-click risk model, `confirm_needed` before irreversible steps, and the fill, pause and ownership checks.

## 5. Answers

- **Allowed:** the operation runs as today.
- **Refused:** exit 4, code `gatekeeper_denied`, reason `not_covered` or `owner_said_no`. The message tells the agent to ask the owner in chat, naming exactly what it will do, then retry. Exit 4 otherwise means "don't retry", so the message and the docs both say retry-after-asking for `not_covered`.
- **Can't judge:** exit 3, code `gatekeeper_unavailable`, when the chat read or the judge fails. Fails closed.
- **No chat:** `which_chat` or `no_chat`.
- A browser plan that stops after some steps says which steps already ran.

## 6. Host setting

One optional switch, for "no exec on my machine":

```
latch serve ~/work --deny-exec              # no agent may exec here
latch serve ~/work --deny-exec=ag_7Hk2      # this agent may not (repeatable)
```

- Default: exec is available, through the gatekeeper.
- Keyed by verified `agent_uid`, never a name. Devices are unaffected.
- It only restricts; it never lets anything past the gatekeeper.
- The value needs `=`, so a shared folder after the flag is never read as an agent id.
- Other per-area switches wait until someone asks.

## 7. Names

A device's name defaults to its hostname, with `--name` to override, and no prompt. A clash stops the login; it never renames or replaces silently. Agents keep their Plow name. A name is a label, never authentication.

## 8. Audit

Every agent operation's audit record gains a `gate` object:
- verdict and the three probabilities, model, Plow request id and `usage.cost`;
- chat uid, the message window read (first and last ids, count, truncated or not), and the ids of the owner messages shown;
- latency.

No chat text and no secret values are recorded. The shared model client is extended to keep Plow's `usage.cost`, which it drops today.

## 9. Cost and speed

Only jev's own speed is measured (p50 135 ms, p95 182 ms). The rest are estimates that G2 measures:

| Case | Judgments | Added cost | Added time |
|---|---|---|---|
| One judgment | 1 | $0.0002–0.0007 | ~0.5 s p50, 1.5 s p95 |
| Browser shopping task (~60 calls: 1 open, 2 plans, 2 fills) | ~5 | $0.001–0.004 | 2–3 s |
| Coding loop, 100 exec calls | 100 | $0.02–0.07 | ~50 s |

If exec cost hurts, the levers are `--deny-exec` now, or a coarser exec description later.

## 10. Known limits (v0, accepted)

- **The judge's own leaks:** acting before the time the owner set, amounts beyond what they said, weak replies like "ok cool" read as yes. These are about 7% of held-out decisions.
- **Single browser actions on an allowed site run unjudged.** `act` and clicks outside a plan rely on the browser's risk check. A click that navigates to a new site isn't treated as opening one.
- **Owner provenance is Plow's classification,** not proven ingress. Messages injected through Plow's test seam with a device or test credential look like the owner's and are not excluded in v0. Proper provenance needs a Plow change and comes later.
- **A cancellation in a different chat with the same agent isn't seen.**
- **"Already done" covers this `serve` run only.**
- **A device login placed where an agent runs bypasses the gate.** Agents must use their own credential; the docs say so.

## 11. Out of scope

Structured ask records, use-once approvals, long-chat retrieval, member approval, per-area switches beyond exec, host-issued pairing for non-Plow agents, MCP.

## Chunks

### Global Constraints

- Product stage: internal tool, early users. Simple and understandable beats complete.
- No Plow changes. Use only routes on Plow `main`.
- Chat text reaches only the judge's input (§3). Never put chat text or secret values in the audit or the logs.
- Fail closed: if the gatekeeper can't judge, nothing is dispatched and no data is returned.
- Devices (the owner's own logins) are never judged.
- Tests and test hosts are headless. Synthetic chats only; never the owner's real chats.

### Chunk G1: Chat context and the v0 judge
Implements: §3, §2.2, §8 (cost field)
Interfaces: consumes Plow agents, chats and messages routes and `POST /v1/decisions` · produces `judge(agent, chat, action) → allow | deny{reason} | unavailable{code}` and wire field `Request.chat`
Done when: fixture tests of synthetic Plow responses cover owner classification, pagination, truncation, and missing, ambiguous and wrong chats; for every probe in the v0 held-out corpus, the Rust judge builds a `/v1/decisions` request whose question, rules, state and threshold are equal to `v0.py`'s (a deterministic comparison, no model call); and one replay of the corpus through the Rust judge on jev-1.14 is reported with false allows and false blocks separately, next to v0's 12/164 and 4/52 (reporting only, not a pass bar).

### Chunk G2a: exec, files and Google
Implements: §4 (exec, files, Google rows), §5, §8
Interfaces: consumes G1's `judge` · produces gated `serve_operation` for agents
Done when: on a headless test host with synthetic chats, a refused exec, file and Google call each dispatch nothing and return no data; an unavailable judge does the same; an owner reply lets the retry through; the same exec twice is judged twice; the audit holds the `gate` object with no chat text or secrets.

### Chunk G2b: browser sites, plans, vault use, code and files
Implements: §4 (browser rows), §5, §8
Interfaces: consumes G1's `judge` · produces gated browser host operations
Done when: on a headless test host with synthetic chats, a refused open, plan, fill, eval, upload and download each dispatch nothing; clicks and page reads inside an allowed session make no judge calls; an owner "stop" refuses the next plan; a real browser shopping task's judge calls, cost and added time are measured and reported against §9.

### Chunk G3: `--deny-exec`, default names, docs
Implements: §6, §7, §5 (docs)
Interfaces: consumes agent identity from the peer lookup · produces the `serve` flag and login naming
Done when: global and per-agent denies hold across agent renames and leave devices alone; `latch serve ~/a --deny-exec ~/b` parses as two folders; a device login with no `--name` takes the hostname and stops on a clash; `--help` and the client docs show deny → ask the owner → retry and say an agent must never use a device login.

Order: G1 → G2a ∥ G2b; G3 any time.
