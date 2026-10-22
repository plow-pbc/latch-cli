# Help for humans and agents: layout research for latch (2026-10-22)

Method: I read latch at f626899 (`cli.rs`, `help.rs`, `SKILL.md`) and diffed real outputs from a clean build of that commit. Outside sources are cited inline.

## What latch does today

- **`--help`, `-h`, `latch help CMD`: one text, two layouts.**
  - `-h` and `--help` are byte-identical. Every added block uses `after_help`, and nothing sets `long_about` or `after_long_help`.
  - `latch help CMD` calls `render_long_help()` on the same clap tree. It differs only in layout (help text on the next line), and every word matches.
- **`help --json` and `help --examples`: from the `ENTRIES` table.**
  - `--json` lists 97 actions and 120 examples with output, side effects, retry and exit codes. Tests keep it in step with the parser.
  - `--examples` is a subset of `--json`.
  - `--json` carries none of the added `--help` text (`EXEC_HELP`, `DO_HELP`, `SERVE_HELP`).
- **`latch skill`: an untested copy of shared guidance.** It prints `SKILL.md`, which repeats about six pieces of agent guidance from `AGENT_HELP`, `MESSAGES_HELP`/`messages::HELP` and `EXEC_HELP`:
  - device logins
  - exit 3 and exit 4 handling
  - steps/stop before resending
  - untrusted content
  - secret-env
  - pointers to the other help surfaces

  The messages "untrusted" note exists in three copies. The secret-env example exists in three versions.

## What others do

- **clap: `-h` is short, `--help` is long.** Each text field has a short and a long form, and the short one is reused when the long one is unset:
  - `about` and `long_about`
  - `after_help` and `after_long_help`
  - `before_help` and `before_long_help`

  `render_help` gives the `-h` text and `render_long_help` the `--help` text. `disable_help_subcommand` turns off clap's own `help` subcommand ([clap `Command`](https://docs.rs/clap/latest/clap/struct.Command.html)).
- **clig.dev:**
  - show help on `-h` and `--help`;
  - for git-like tools, `app help` and `app help sub` should also work;
  - "Lead with examples", and put large example sets somewhere else;
  - `--json` for machine-readable output;
  - link to the web docs from help ([clig.dev](https://clig.dev/)).
- **cargo:** `cargo help build` and `cargo build --help` are two ways to reach one thing ([cargo help](https://doc.rust-lang.org/cargo/commands/cargo-help.html)). Its machine output is versioned: `--format-version 1` pins the format, new fields can be added, and an incompatible change bumps the version ([cargo metadata](https://doc.rust-lang.org/cargo/commands/cargo-metadata.html)).
- **ripgrep 14:** generates its man page and shell completions itself (`rg --generate man`) from the same flag definitions that parse ([CHANGELOG 14.0.0](https://github.com/BurntSushi/ripgrep/blob/master/CHANGELOG.md)).
- **jj:**
  - `jj util markdown-help` (via `clap-markdown`) generates the web CLI reference from clap. The reference was kept as an `insta` snapshot ([jj commit](https://git.joshthomas.dev/mirrors/jj/commit/52c415e)).
  - The site says the reference "does not match the `jj help` output exactly" and that `jj help <COMMAND>` is the more authoritative source ([jj CLI reference](https://docs.jj-vcs.dev/latest/cli-reference/)).
  - `jj help -k` shows topics that are not commands.
- **clap_mangen:** renders a `clap::Command` as roff. It suggests an xtask over `build.rs` ([clap_mangen](https://docs.rs/clap_mangen/latest/clap_mangen/)).
- **uv:** its CLI reference is evidently built from clap doc comments: a uv commit fixed rendering with `verbatim_doc_comment` ([commit](https://app.semanticdiff.com/gh/astral-sh/uv/commit/7a18e4429d6750365c2f2ad98907a90d97e67fb7)). I did not confirm a staleness test.
- **gh:** `--json` with no field list prints the available fields, and `--jq`/`--template` shape the result ([gh formatting](https://cli.github.com/manual/gh_help_formatting)).
- **Agent-facing formats:**
  - **MCP tools:** `name`, `description`, `inputSchema`, optional `outputSchema` and behaviour `annotations` (read-only, destructive, idempotent hints). Clients must treat annotations as untrusted unless the server is trusted ([MCP tools](https://modelcontextprotocol.io/specification/2025-06-18/server/tools)).
  - **Agent Skills:** load in stages. `name` and `description` (~100 tokens) are always loaded. The `SKILL.md` body (under 5k tokens) loads when triggered. Reference files load only when read ([Agent Skills](https://platform.claude.com/docs/en/agents-and-tools/agent-skills/overview)).
  - **llms.txt:** a short curated Markdown index of links ([llmstxt.org](https://llmstxt.org/)).
  - **AGENTS.md:** a "README for agents" covering a repo's build and test steps ([agents.md](https://agents.md/)). It is for working on a codebase, not for using a CLI, so neither it nor llms.txt fits latch's binary.

## Recommendation for latch

**1. One home for each kind of fact.**

| Fact | Home | How the others point to it |
|---|---|---|
| A command's purpose, flags, defaults, env vars | clap doc comments in `cli.rs` | `--json` `inputs` already reads them |
| Output, side effects, retry, owner_only, connector, examples | `ENTRIES` | `--help` renders `ENTRIES` examples into `after_long_help` |
| Exit codes and what they mean | `Exit::meaning` (`failure.rs`) | `--json` `exit_codes`; root `--help` and `latch skill` list them from `Exit::ALL` |
| A command's grammar and limits (`do` checks/facts, exec stdin and output limits, serve notes) | that command's `after_long_help` | `latch skill` names the command, it does not restate it |
| Cross-command judgement: when to use what, gatekeeper refusals (`not_covered`, `confirm_needed`, `calendar_conflict`), untrusted content, secrets policy, browsing and buying, the texted sign-in code | `SKILL.md` only | Root `--help` is one line: "Agents: run `latch skill` first; `latch help --json` describes every command." |

`-h` stays terse (`about` and flags). `--help` and `latch help CMD` stay one long text: keep the alias, since clig.dev and cargo both expect it. `help --json` is the agent's tool list, latch's version of MCP `tools/list`. Give it a `format_version` the way cargo does, and consider mapping `Effect` to `read_only` and `idempotent` flags in the MCP style.

**2. Keep the surfaces from drifting.**
- Generate rather than copy. `latch skill` already adds the version at runtime. Have it also add the exit code list from `Exit::ALL`, and build the `--help` examples from `ENTRIES`. That is ripgrep's one-source idea without committing generated files.
- Extend the existing example test so that every `` `latch …` `` command line in `SKILL.md` and in each `after_long_help` must parse against the clap tree, just as the `ENTRIES` examples must today.
- Add a test that every error code `SKILL.md` names (backticked snake_case next to "exit") is one latch can produce. This only works if the codes can be enumerated; today they are string literals passed to `Failure`.
- Snapshot root `--help`, `-h` and `latch skill` (as jj does with `insta`), so a copy change shows up in review.

**3. Delete.**
- `AGENT_HELP` except its pointer line. Its device-login, exit 3/4 and steps-retry text is in `SKILL.md`.
- `MESSAGES_HELP` (root). `messages::HELP` on the subcommand and `SKILL.md` already say it.
- `EXEC_HELP`'s "Exit code and output" exit-code sentence: replace it with the generated list. Also its `Example:` line, which duplicates `ENTRIES`.
- `DO_HELP`'s hand-written examples, in favour of `ENTRIES` (move the missing ones such as `--fact` and `--dry-run` into `ENTRIES`). Keep its grammar prose.
- `help --examples`: it is a subset of `--json`, and once `--help` shows the examples it is a third route to them. Update `SKILL.md:26-28` to match.
- Not needed now: man pages and markdown references (`clap_mangen`, `clap-markdown`). The audience is agents running the binary. Add them later from the same clap tree if humans ask.
