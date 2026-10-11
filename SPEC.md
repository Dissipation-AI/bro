# bro — spec

`bro` turns what you meant into a shell command, using what's on your screen.

```
~/code/site $ bro kill that ghostty process
~/code/site $ pkill -i ghostty█                       ← pre-filled, not run. Enter runs it.
  bro: stops every process whose name contains "ghostty" (3 running)
```

Type or dictate `bro` followed by plain English or half a command, then press Enter. `bro` collects
context (your last few commands **and their output**, folder, environment, running processes,
open tabs), asks your LLM, and puts the answer **in your next prompt**. Nothing runs until you
press Enter again.

Compared with `claude -p '…'`: `bro` sees your terminal history, works with a small local model
(e.g. Qwen on localhost), and starts in milliseconds because it's one small native binary with no
runtime to boot.

## Goals

1. **Fast:** < 10 ms from Enter to the request leaving the machine (context gathering included).
   Total time is then up to the model. With a local small model, aim for an answer within 1 second.
2. **Voice-friendly:** works with dictation tools like Handy. `Bro, open a browser searching for
   salsa dancing videos.` must work despite the capital letter, comma, apostrophes and final full
   stop.
3. **Never runs anything by itself.** It only fills in your command line prompt.
4. **Works with dumb models.** Prompting and output parsing are designed for 3–8B local models.
5. **Works anywhere, better with ctx.** `bro` is a separate tool and doesn't need `ctx`. In any
   terminal it uses shell history. If `ctx` is installed and you're in a CTX tab, it also asks
   `ctx` for recent command output and your other tabs, the same way it asks `git` for the branch.

## Non-goals

- Chat, multi-turn agents, or running tools in a loop. One request in, one command out. (cleaver users can use the # to leave a comment in "response" in the history, which bro picks up)
- Its own model hosting. You bring an endpoint.
- bash/fish (v1 is zsh only).

## How it's triggered

Natural language is full of characters zsh would choke on (`what's` opens a quote, `?` is a glob,
`!` is history expansion). So `bro` is **not** an ordinary command. It's caught **before zsh
parses the line**, by a ZLE widget that wraps `accept-line`:

1. When you press Enter, the widget checks whether the line starts with the word `bro`, in any
   case and followed by optional punctuation: `bro`, `Bro`, `BRO`, `bro,` `Bro.` `bro:` all match.
   Matching is a regex in the widget: `^[[:space:]]*[Bb][Rr][Oo][[:punct:]]*([[:space:]]|$)`.
2. If it doesn't match, Enter behaves normally. Zero cost.
3. If it matches, the widget takes the rest of the line verbatim (quotes, `?`, `!` and all), adds
   it to history as typed, and runs the `bro` binary with the request on stdin. It shows
   `bro is cooking…` under the prompt (`zle -M`), with Ctrl-C to cancel.
4. The answer replaces the line (`BUFFER`), with the cursor at the end, and a one-line explanation
   is shown under the prompt. Press Enter to run, or edit it first.

Also:

- **Ctrl-G** (configurable): run `bro` on whatever is currently typed, with no `bro` prefix needed.
  This is for "I've typed half a command, finish it": `git rebase onto main but keep my` → Ctrl-G.
- **`bro` as a plain command** (outside zsh, in scripts, or before shell integration is installed)
  prints the suggested command to stdout, plus the explanation on stderr. `bro --run` doesn't
  exist, on purpose.
- Install: `bro install` adds one line to `~/.zshrc` (`eval "$(bro init zsh)"`). Inside CTX the
  `ctx` shell integration loads it automatically if `bro` is installed.

## Context it gathers

Gathered in parallel, each source with a time limit (default 30 ms) and a size budget. A slow
source is skipped, not waited on. Total prompt budget is configurable (`max_context_tokens`,
default 6,000, since small models have small windows). Sources in priority order, trimmed from the
bottom when over budget:

| Source | In a CTX tab (`ctx` on PATH) | Elsewhere |
|---|---|---|
| Your request + the current line | ✓ | ✓ |
| Last 3 commands (excludes empty lines), exit status, **and their output** (tail-trimmed to ~60 lines each) | `ctx read last 3 commands as json` | commands only, from zsh history |
| Current folder, git branch + short status, `ls` of the folder (first 50 entries) | ✓ | ✓ |
| Your open tabs: number, name, folder, running command | `ctx list as json` | — |
| Running processes: top 15 by CPU and by memory, plus the shell's `jobs` | ✓ | ✓ |
| Environment variables (redacted, see Privacy) and alias/function names | ✓ | ✓ |
| Installed apps (`/Applications`, `~/Applications`), default browser | cached | cached |
| Project index: git repos and top-level folders under `~` (depth 3) | cached | cached |
| Past `bro` requests and the command you actually ran afterwards (last 20, most relevant first) | ✓ | ✓ |
| Saved tabs matching the request (e.g. "how did I deploy last time"): top 3 hits from `ctx find … as json`, commands only | ✓ | — |
| **Man pages** for commands named in the request or current line (see below) | ✓ | ✓ |
| macOS version, CPU arch, shell, `$TERM_PROGRAM` | ✓ | ✓ |

- **Man pages.** When the request or the half-typed line names a command that exists on `PATH`
  (`bro rsync but skip node_modules`, `tar -x… finish this`), `bro` includes the relevant part of
  its man page, so the model uses the flags *this* machine's version supports (macOS's BSD `sed`,
  `find`, `date` differ from the GNU versions small models mostly saw in training):
  - located with `man -w <command>`, rendered to plain text with macOS's built-in `mandoc`;
  - trimmed to NAME, SYNOPSIS, and the option entries whose text best matches the request's words.
    Whole man pages are far too big, especially for a 4K-context model like apfel;
  - if installed, the command's `tldr` page is added too (short, example-driven, ideal for small
    models);
  - **never** by running `<command> --help`, since that would execute arbitrary programs just to
    gather context;
  - rendered and trimmed pages are cached in `~/Library/Caches/bro/man/`, keyed by the man file's
    path and modification time, so it's a file read after the first time.
- Processes are read via `libproc` directly (no spawning `ps`). Git is read with `gix` limited to
  HEAD + status, or skipped if the repo is large and it would blow the time limit.
- **Cache** (`~/Library/Caches/bro/`): apps list and project index are refreshed in the background
  at most every 10 minutes, so requests never wait on a filesystem walk.
- **Learning from what you do:** after `bro` fills your prompt, the zsh integration records what
  you actually ran (maybe edited) and its exit status. Those pairs are sent back as examples on
  similar future requests. This is how "open my personal sysadmin agent" learns what that means
  for you. Stored at `~/Library/Application Support/bro/history.jsonl`.

## Talking to the model

- **Endpoint:** any OpenAI-compatible `/v1/chat/completions`, e.g. Apple's on-device model via
  [apfel](https://apfel.franzai.com/) (`apfel --serve`, macOS 26+, free and on-device, but only
  4,096 tokens for prompt and answer combined), llama.cpp server, Ollama, LM Studio,
  vLLM, or a cloud provider). Streaming (SSE), so the explanation can appear early.
- **HTTP client:** `ureq` (blocking, rustls, no async runtime) for minimal startup and binary size.
- **Output format:** the model is asked for JSON:
  `{"command": "...", "explanation": "...", "confidence": "high|low"}`. When the server supports it,
  `response_format` with a JSON schema (llama.cpp grammar / Ollama `format`) forces valid output.
  Otherwise bro parses leniently: JSON first, then the first fenced code block, then the first line.
- **Prompt:** a short fixed system prompt (kept in `bro/prompts/system.md`, embedded at build time)
  with a handful of built-in examples covering the target use cases:
  - kill a process by description → `pkill -i ghostty`
  - web search → `open "https://search.brave.com/search?q=salsa+dancing+videos"` (search engine
    and browser from config)
  - find a terminal session → `ctx show "CTX spec writing"` (from the open-tabs context)
  - open an app or project → `open -a "…"` / `cd ~/…` / `ctx open tab at ~/… named "…"`
  - fix the last failed command, using its error output
  - complete a half-typed command
- When the model is unsure, it says so in `explanation` and still gives its best command. A
  question back to the user isn't allowed (one shot).
- `temperature = 0` by default.

## Called by other tools

`bro` is a single-purpose Unix tool: natural-language intent in, one terminal command out. Other
programs can use it the same way a person does, with no special integration:

```
$ echo "list the five biggest files in this folder" | bro --json
{"command": "du -ah . | sort -rh | head -n 5", "explanation": "...", "confidence": "high", "dangerous": false}
```

- Request on stdin (plain text, any length within the context budget). Without `--json`, the
  command goes to stdout and the explanation to stderr. Exit status 0 if a command was produced,
  1 if not.
- `bro` doesn't know who's calling. A caller that needs a command for *its own* tool puts what
  `bro` needs to know (e.g. that tool's grammar or help text) into the request. That's how `ctx`
  asks for a corrected `ctx` sentence when one doesn't parse (see ctx spec, "assist").
- `bro` builds commands, nothing else. No summarisation, no reading long transcripts, no
  question-answering. Those belong to the tools that need them, with models suited to them.

## Safety

- Never executes. The only output is text placed in your prompt (or printed).
- **Danger flag:** a local, model-independent check marks risky commands (`rm -r`, `sudo`, `dd`,
  `mkfs`, `chmod -R`, `> ` onto an existing file, `curl … | sh`, `kill` of pid 1 or system
  processes, `git push --force`, `git reset --hard`…). The explanation line turns red and starts
  with `⚠`. Nothing is blocked; you just can't miss it.
- Multi-line or `;`/`&&`-chained output is allowed but shown in full; nothing hides past the edge
  of the screen.

## Privacy

Your terminal can contain secrets, so **nothing key-like ever goes into the prompt**, even when the
endpoint is a local model. Redaction is always on and can't be turned off. It applies to every
context source, not only output: environment values, shell history and past commands (e.g.
`export OPENAI_API_KEY=sk-…`), command output, saved-tab search hits, and `bro`'s own history file.

- Environment variables: names are always sent, but **values** only for an allowlist (`PATH`,
  `HOME`, `SHELL`, `LANG`, `EDITOR`, `PWD`, `TERM*`, `CTX_*`, version-manager vars…). A variable
  whose name matches `KEY|TOKEN|SECRET|PASS|AUTH|CRED|SESSION|COOKIE` is never sent with a value,
  even if allowlisted.
- Text (commands, output, history) is scanned, and matches are replaced with `[redacted]`:
  - known secret shapes: AWS keys, `sk-…`/`sk-ant-…`, GitHub/GitLab/Slack tokens, JWTs, PEM
    blocks (`-----BEGIN … PRIVATE KEY-----`), `Authorization:`/`Bearer` headers, URLs with
    `user:password@`;
  - assignments: `NAME=value`, `--token value`, `-p value`, `password: value` where the name looks
    secret, for both the shell syntax and JSON/YAML/`.env` forms;
  - **unknown formats:** any token of 20+ characters with high character entropy and mixed
    character classes, unless it's clearly a path, a URL without credentials, or a git hash
    (40 hex chars in a git context).
- Redaction is a single function (from `shared/redact`, called in `src/safety.rs`) with its own test corpus (true positives and
  must-not-redact cases such as paths, hashes, UUIDs in command names). It runs last, on the fully
  assembled prompt, so no source can bypass it.
- If the configured endpoint is not on `localhost`/`127.0.0.1`/a private network address, `bro
  config` warns once that terminal output will leave the machine.
- `bro --show-prompt <request>` prints exactly what would be sent, without sending it.

## Configuration

`bro config` is an interactive setup (endpoint, model, test request, search engine). It offers
presets that fill in the endpoint and a fitting context budget: **apfel** (localhost, budget
~2,500 prompt tokens to leave room for the answer), **Ollama**, **llama.cpp**, **LM Studio**,
**custom**. With a 4K-context model, sources are trimmed hard: the last command's output is
capped at ~20 lines, and only the top 5 processes and 5 history examples are included. It writes
`~/.config/bro/config.toml`:

```toml
endpoint = "http://localhost:8080/v1"
model = "qwen3-8b"
api_key_env = ""                  # name of an env var holding the key, never the key itself
timeout_ms = 15000
temperature = 0
max_context_tokens = 6000
search_url = "https://search.brave.com/search?q={query}"
browser = "default"               # or an app name, e.g. "Brave Browser"
complete_key = "^G"
history_examples = 20
```

`bro config --check` sends a tiny request and reports latency, so you know your endpoint works.

## Crate layout

```
bro/
  src/main.rs       args, stdin request, output
  src/context/      one module per source (ctx, history, processes, git, env, apps, projects)
  src/llm.rs        OpenAI-compatible client, streaming, lenient parsing
  src/safety.rs     danger flags, redaction
  src/cache.rs      background refresh of apps/project index
  shell/bro.zsh     the ZLE widget (embedded; printed by `bro init zsh`)
  prompts/          system prompt + built-in examples
```

No dependency on any `ctx` crate. `bro` talks to `ctx` only through the `ctx` command line and its
documented `as json` output, like any other tool. If `ctx` isn't installed, or you're not in a CTX
tab (`CTX_TAB` unset), those sources are simply absent. Redaction rules come from the monorepo's
`shared/redact` library crate, compiled in, with no runtime dependency.

## Stack

| Concern | Choice | Why |
|---|---|---|
| Language | Rust, edition 2024, pinned stable toolchain | startup in milliseconds, one small binary |
| HTTP | `ureq` with rustls, SSE parsed by hand | blocking, no async runtime, small |
| JSON | `serde_json` | model I/O and `--json` |
| Processes | `libproc` via small FFI | no `ps` spawn |
| Git | `gix` with minimal features, time-limited | branch + short status without spawning `git` |
| Man pages | `man -w` + `mandoc -T utf8` (both ship with macOS) | exact local flags, no execution of the command |
| Redaction | `shared/redact` (compiled in) | same rules as `ctx ask` |
| Config | `toml` + `serde` | |
| Arg parsing | `lexopt` | `bro` has only a handful of flags; tiny |

No `tokio`, no `clap`. Every dependency is justified and checked (`cargo deny`, `cargo audit`).
Budgets enforced in CI: binary < 5 MB, startup to request-sent < 10 ms on a cached run.

## Testing

**Unit tests** (`cargo test`, every commit):

- trigger matching: every `bro` variant (`Bro,` `bro.` `BRO:`…) and lines that must *not* match
  (`brown`, `broadcast`);
- each context source: output shape, time limit honoured, budget trimming order;
- man-page extraction: picks the right options for sample requests, from fixture man pages
  (BSD and GNU variants);
- redaction: the shared corpus, plus bro-specific cases (history lines, env dumps);
- danger flags: a table of commands → flagged or not;
- lenient model-output parsing: valid JSON, JSON in prose, fenced code, bare line, garbage.

**Integration tests:** a fake OpenAI-compatible server (in-process) that records the exact prompt
and replies with canned answers. These assert the prompt contents, redaction and timing. The zsh
widget is tested by driving a real `zsh` in a PTY (`expect`-style), including Ctrl-C cancel and
apostrophes/`?`/`!` in requests.

**Model evals:** the evaluation set below, run manually or nightly against configured endpoints
(apfel, a small Qwen); results tracked over time, not a CI gate.

## Phases

1. `bro` as a plain command: config, endpoint client, context from history/folder/env/processes.
2. zsh widget: `bro`-prefix catching, voice-style variants, prompt fill, Ctrl-G completion.
3. Optional `ctx` context: command outputs and open tabs, via the `ctx` CLI.
4. Learning from what you actually ran; app and project caches; man/tldr pages.
5. Prompt tuning against a fixed eval set (below) on 2–3 small local models.

## Evaluation set

`bro/evals/cases.toml`: request + context snapshot → acceptable commands (regex). Run with
`bro eval --endpoint …`. Starter cases:

- "kill that ghostty process" (with ghostty in the process list)
- "Bro, open a browser searching for videos about salsa dancing."
- "find the terminal session where I am working on project ctx" (with a tab list)
- "open my personal sysadmin agent" (with a project index + past history)
- "why did that fail" / "fix it" (after a failed `cargo build` with errors in output)
- "git rebase onto main but keep my" (half-typed command)
- "what's using port 3000" (apostrophe + question mark)
- "delete all the node_modules folders under here" (must be danger-flagged)
