# CLI output: machine vs human, errors, copy. Research for latch (2026-10-22)

Method: I ran `--help` and real error cases on the installed `gh` and `aws` 2.37.12, `kubectl`, `docker`, `terraform` 1.16.5, `tailscale` and `op`. The rest come from docs (cited). Rows marked † are from docs or third-party mirrors only, not run here.

| CLI | Machine output | Selected by | Default when piped | Consistent across commands? | Errors (stream, shape, exit) | Non-interactive auth / prompts |
|---|---|---|---|---|---|---|
| gh | `--json fields` + `--jq`/`--template` | flag; `GH_FORCE_TTY`, `GH_PROMPT_DISABLED` | tab-separated text, no header or colour (seen) | No: only some commands | plain text on stderr; exits 0, 1, 2 (cancel), 4 (auth) (`gh help exit-codes`) | `--with-token` on stdin, `GH_TOKEN`; device code with `--web --clipboard`; a missing flag fails with `must provide --title and --body … when not running interactively` |
| kubectl | `-o json\|yaml\|jsonpath\|name…` | flag | same text table | Yes, on get/describe-style verbs | plain `Error …` on stderr, even with `-o json`; exit 1 (seen) | kubeconfig; `get -w --output-watch-events` streams JSON |
| docker | `--format json` / Go template | flag | table | Most list/inspect commands | plain text on stderr; `inspect` still prints `[]` on stdout; `run` uses 125/126/127 | `docker events --format '{{json .}}'` streams |
| terraform | `-json`: JSON lines with `@level`, `@message`, `type`, and a first `version` line carrying a `ui` schema version | flag | text, colour even when piped (ignores `NO_COLOR`; needs `-no-color`) | plan/apply/validate/show/test | in `-json` mode, a `diagnostic` line on **stdout** {severity, summary, detail}; exit 1 | `-input=false`; [machine-readable UI doc](https://developer.hashicorp.com/terraform/internals/machine-readable-ui) |
| aws | `--output json\|text\|table\|yaml` | flag, `AWS_DEFAULT_OUTPUT`, config file | json | Yes, global | `--cli-error-format json\|yaml…` (global); exits 252 syntax, 253 config, 254 service, 255 other (`aws help return-codes`) | env/SSO credentials; `--no-cli-pager` |
| gcloud† | `--format json\|yaml\|value(…)` | flag, config property | human table | Yes, global | `ERROR: (gcloud.x) …` on stderr | `--quiet` / `CLOUDSDK_CORE_DISABLE_PROMPTS=1`: a prompt that needs real input becomes an error ([ref](https://docs.cloud.google.com/sdk/gcloud/reference)) |
| op (1Password) | `--format json` | flag **or `OP_FORMAT` env** | human | Yes, global | `[ERROR] <time> message` on stderr (seen) | service-account token env |
| tailscale | `--json` ("WARNING: format subject to change") | per-command flag | text | No | text | `--auth-key`, `--qr`, `--timeout` on `up`/`login` |
| supabase† | `-o env\|pretty\|json\|toml\|yaml` | global flag | pretty | Partly | text | [global flags](https://supabase.com/docs/reference/cli/global-flags) |
| flyctl†, vercel†, wrangler†, heroku†, stripe† | per-command `-j/--json` / `--format=json` | flag; wrangler also `WRANGLER_OUTPUT_FILE_PATH` (NDJSON to a file) | text | No; [flyctl](https://fly.io/blog/flyctl-meets-json/) bugs where `-j` printed text; vercel has prompts with no skip flag† | text | `--yes`, token env (vercel: never `--token` in argv, CVE-2026-44479) |
| Arcjet CLI† (built for agents) | JSON | `--output json`, **else JSON whenever stdout isn't a tty** | JSON | Yes | stderr `{error, code, remediation}`; exits 1 general, 2 auth, 3 input, 4 confirmation needed | mutations return `confirmation_required` with the exact `confirmCommand`; no prompts ([post](https://arcjet.com/blog/designing-a-cli-for-ai-agents)) |
| clispec.dev† (agent CLI spec, v0.3 candidate) | `-o auto\|json\|ndjson\|text`, `auto` = tty detection, the flag always wins | flag | structured | — | last stderr line `{"error":{"kind","message","hint","details"}}`, each kind with a declared exit code; `tty_required`, `confirmation_required` | `schema` command for introspection; streams are flushed NDJSON ([spec](https://clispec.dev/)) |

**1. Choosing human vs machine output.** Every established CLI defaults to text and makes JSON opt-in with a flag. Only the global-flag tools are consistent across commands: aws, gcloud, op, kubectl, supabase. Per-command `--json` (gh, flyctl, tailscale, wrangler) drifts. Only `op` (`OP_FORMAT`) and aws (`AWS_DEFAULT_OUTPUT`) take an env var. The agent-first designs (Arcjet, clispec) flip it: structured output whenever the stream isn't a tty, and an explicit flag that always wins.

**2. Error contract.** Most established CLIs print plain-text errors and give little exit-code structure (gh: 4 = auth). The good contracts:
- **aws:** a global `--cli-error-format json`, plus documented exit classes.
- **terraform:** an error is just another typed JSON line in the stream.
- **clispec and Arcjet:** a JSON envelope with a stable code/kind, a message, a hint (remediation) and an exit code per kind.

None makes a program parse prose.

**3. Interactive commands.** The patterns:
- **A flag for every prompt:** gh's `--title`/`--body`, vercel `--yes`.
- **A global no-prompt switch:** gcloud `--quiet` turns a needed prompt into an error.
- **Secrets by stdin or env, never argv:** `gh auth login --with-token`, `GH_TOKEN`; vercel's CVE is what argv tokens cost.
- **Device-code or URL output a GUI can show:** `gh --web --clipboard`, `tailscale up --qr`.
- **A typed refusal instead of hanging:** clispec's `tty_required`, Arcjet's `confirmation_required`.

**4. Streaming.** NDJSON is universal: kubectl `-w`, `docker events`, terraform `-json`, wrangler's output file, clispec `ndjson`. The best version is terraform's: every line has a `type`, the first line is a `version` with a schema version, and clients ignore unknown types and fields.

**5. Copy.** clig.dev: "Display output on success, but keep it brief", "Catch errors and rewrite them for humans", "Suggest commands the user should run", "Put the most important information at the end", colour "with intention" and off under `NO_COLOR` or when not a tty. Real messages that work:
- `✓ Logged in to github.com account plonkus (keyring)` (gh): a symbol, what happened, the one fact that matters.
- `must provide --title and --body (or --fill …) when not running interactively` (gh): the reason plus the fix.
- `Error: No configuration files … create a Terraform configuration file (.tf file) and try again.` (terraform): a summary line, then a short detail with the next step.
- `docker: … repository does not exist or may require 'docker login'` (docker): names the likely fix.
- Counter-example: `GraphQL: Could not resolve to a PullRequest with the number of 999999999. (repository.pullRequest)` (gh) leaks internals.

## 6. Recommendation for latch

**Output rule: data is JSON, always.** Keep stdout JSON for every data command regardless of tty, as main does now. Latch's readers are agents and a GUI, and tty-dependent stdout is the bug class to avoid (the per-command `--json` CLIs show the drift).
- **Global switch:** add `--json` plus `LATCH_JSON=1` as one global switch that also makes errors JSON at a tty and turns the tty-only renderings into JSON. Those renderings are `login`'s line, `setup` and bare `latch`.
- **Human views are opt-in:** extend the `--pretty` pattern `status` already has, for people.
- **Interactive commands** (`setup`, `serve`, `dashboard`, `vault add`) without a tty exit 2 with `tty_required` and name the non-interactive command. `setup` gets `--events` JSON lines like `login --events`, so the GUI can drive it.
- **Tradeoff:** a person at a terminal sees JSON from data commands. They're a minority who mostly run setup, and `--pretty` or `jq` covers them. An env var also leaks into child processes; that is acceptable because latch's children are latch.

**Error contract.** Every error is one line on stderr: `{"error":{"code","message","hint"?,"exit",…details}}`.
- **Codes:** `code` is stable and listed with its exit code in `help --json`.
- **Exit codes:** keep the 0–5 classes. Argument validation is always 2 (the audit found `bad_secret_env` = 1 and `bad_path` = 4).
- **Plain text at a tty:** print `message` and then `hint`, unless `--json` is set.
- **Gaps to fix:** `setup`'s text error and `help frob` printing JSON at a tty.
- **Tradeoff:** `exit` inside the JSON duplicates the process status, but logs and the GUI keep it.

**Streams.** `events`, `audit show -f` and `login --events`/`setup --events` are NDJSON. Every line has `type`, the first line is `{"type":"version","schema":N}`, and readers ignore unknown types and fields (terraform's guarantee). `logs` stays text: it is a log.

**Copy style guide.**
1. One sentence. Say what happened or what is wrong, then a next step only if there is one: `` `latch serve` starts one ``.
2. Lead with the outcome and put the fix last, as a runnable command in backticks.
3. Name things as the user knows them (host name, folder, `--write`); never internals (HTTP status, struct, iroh).
4. Never repeat what the user just typed, and never explain the design in an error.
5. Success is silent or one line at a tty (`Logged in as studio.`); detail goes in the JSON.
6. At most ~100 characters for a human line; longer help lives in `latch help CMD`.
7. Colour and symbols (`✓`, `!`) only on a tty, never under `NO_COLOR`, and never as the only signal.
8. Same verb, same word everywhere: `host`, `serve`, `turn on`/`enable`. Pick one and lint it in tests.
9. A refusal says who can change it: "the owner turns it on with `latch plugins enable google`".

Sources: the commands above (`gh help formatting`, `gh help exit-codes`, `gh help environment`, `kubectl get --help`, `docker ps --help`, `terraform -help plan`, `aws help return-codes`, `tailscale up --help`, `op --help`), plus [clig.dev](https://clig.dev/) and the pages linked in the table and text: [Vercel CVE-2026-44479](https://advisories.gitlab.com/pkg/npm/vercel/) and the [wrangler output-file changelog](https://developers.cloudflare.com/changelog/post/2025-11-03-wrangler-output-file/).
