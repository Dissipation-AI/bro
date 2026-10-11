<div align="center">

# bro

**Say what you want. Get the command. Press Enter.**

`bro` turns plain English into a shell command, using the context of your terminal session.<br>
It fills your next prompt and **never runs anything by itself**.

[![ci](https://github.com/Dissipation-AI/bro/actions/workflows/ci.yml/badge.svg)](https://github.com/Dissipation-AI/bro/actions/workflows/ci.yml)
[![licence: Apache-2.0](https://img.shields.io/badge/licence-Apache--2.0-blue.svg)](LICENSE)
![macOS](https://img.shields.io/badge/macOS-zsh-black?logo=apple)
![Rust](https://img.shields.io/badge/Rust-one%204%20MB%20binary-orange?logo=rust)

</div>

```console
~/code/site $ bro kill that ghostty process
~/code/site $ kill 812█                                 ← filled in, not run. Enter runs it.
  stops the Ghostty process with pid 812
```

```console
~/code/api $ cargo build
error[E0432]: unresolved import `serde`
~/code/api $ bro why did that fail, fix it
~/code/api $ cargo add serde█
  adds the missing serde dependency
```

```console
~/talks $ ffmpeg -i talk.mov█                            ← half-typed, then Ctrl-G
~/talks $ ffmpeg -i talk.mov -c copy talk.mp4█
  remuxes to mp4 without re-encoding
```

---

## Contents

- [Why bro](#why-bro)
- [Install](#install)
- [Using it](#using-it)
- [Pipe things into bro](#pipe-things-into-bro)
- [Talk to it: voice dictation](#talk-to-it-voice-dictation)
- [Pick a model](#pick-a-model)
- [What bro looks at](#what-bro-looks-at)
- [Safety](#safety)
- [Privacy](#privacy)
- [From scripts and other tools](#from-scripts-and-other-tools)
- [Configuration](#configuration)
- [bro.md: notes for bro](#bromd-notes-for-bro)
- [How it works](#how-it-works)
- [Evals](#evals)
- [Development](#development)

---

## Why bro

|  |  |
|---|---|
| 🧠 **It knows your terminal session** | Your last commands *and their output*, the folder, git branch, running processes, environment, installed apps, your projects. "Fix it" and "kill that" just work. |
| ⚡ **It's fast** | One native binary, no runtime. Under **10 ms** from Enter to the request leaving your Mac. With Gemini Flash-Lite the whole answer arrives in about **0.9 s**. |
| ✋ **It never runs anything** | The command lands in your prompt. You read it, edit it if you like, and press Enter yourself. Risky commands are flagged in red. |
| 🎙️ **It's voice-friendly** | `Bro, open a browser searching for salsa dancing videos.` works, capital letters, commas, apostrophes and full stop included. |
| 🏠 **It runs on your Mac if you want** | Works with Apple's on-device model (free and private), Ollama, llama.cpp, LM Studio, or any cloud endpoint that speaks the OpenAI API. |
| 📚 **It learns your habits** | When you run (or edit) what bro suggested, it remembers. Next time "open my sysadmin agent" means what *you* meant last time. |

---

## Install

You need macOS, zsh (the macOS default) and [Rust](https://rustup.rs) 1.99 or newer.

```sh
cargo install --locked --git https://github.com/Dissipation-AI/bro
bro install       # adds one line to ~/.zshrc
bro config        # pick a model (see "Pick a model" below)
```

Open a new terminal tab and type `bro` followed by anything.

Works in **Ghostty**, **iTerm2** and **Terminal.app**, alongside each terminal's own shell
integration (all three are tested).

> **Firewall?** If you run LuLu or Little Snitch, allow `bro` the first time it connects to a cloud
> model. If a connection is blocked, bro tells you so instead of hanging.

---

## Using it

| You do | What happens |
|---|---|
| Type `bro <what you want>`, press **Enter** | The command appears in your prompt as it streams in, with a one-line explanation under it. |
| Press **Enter** again | The command runs. That's the only way anything runs. |
| Edit it first | It's an ordinary command line. Change anything. |
| Press **Ctrl-C** while bro is thinking | Cancels and puts your words back so you can reword them. |
| Type half a command, press **Ctrl-G** | bro finishes it. No `bro` prefix needed. |

The trigger is forgiving: `bro`, `Bro,` `BRO:` and `bro.` all work. Words like `brown` or
`broadcast` don't trigger it, and lines that don't start with `bro` cost nothing.

Because bro catches the line *before* zsh parses it, you can use any punctuation:
`bro what's using port 3000?` works without quoting.

### Things to try

| Ask | bro answers (real output, Gemini Flash-Lite) |
|---|---|
| `bro what's using port 3000?` | `lsof -nP -iTCP:3000 -sTCP:LISTEN` |
| `bro replace foo with bar in config.yml` | `sed -i '' 's/foo/bar/g' config.yml` (the macOS form) |
| `bro delete all the node_modules folders under here` | `find . -name "node_modules" -type d -prune -exec rm -rf '{}' +` ⚠ |
| `bro fix it` (after a Python `ModuleNotFoundError`) | `pip3 install -r requirements.txt` |
| `Bro, open a browser searching for videos about salsa dancing.` | `open "https://search.brave.com/search?q=videos+about+salsa+dancing"` |
| `bro show files I changed today in this project` | `find . -type f -mtime 0` |
| `Bro, show me the tab with the dev server.` | `ctx show "Dev server"` (in [CTX](#ctx)) |

---

## Pipe things into bro

Sometimes bro needs to *look* at something before it can answer: a config file, a log, the
output of `ps`. Pipe it in:

```console
~ $ cat ~/.config/bro/config.toml | bro set the web search to google
endpoint = "https://generativelanguage.googleapis.com/v1beta/openai"
model = "gemini-3.5-flash-lite"
search_url = "https://search.brave.com/search?q={query}"        ← passed through, like tee
…
~ $ sed -i '' 's|search_url = .*|search_url = "https://www.google.com/search?q={query}"|' ~/.config/bro/config.toml█
  bro: switches the web search to Google
```

- **It works like `tee`:** whatever you pipe in is shown as it arrives, so you see what bro sees.
- **The answer lands in your next prompt**, exactly as with `bro …` on its own. Nothing runs.
- **bro remembers it.** The last few things piped into bro in this terminal (redacted, for up to
  a day) are part of the context for your later requests, so "now change the model too" works.
  Outside [CTX](#ctx), this is how bro gets to see a command's output at all.
- **bro writes these for you.** When it needs to see something it can't, it suggests the pipe
  itself: ask *"fix the port in docker-compose.yml"* and you may get
  `cat docker-compose.yml | bro change the web port to 8080`. Press Enter, bro reads the file and
  answers again. Requests can chain like this as many times as it takes.

Ask bro about bro (*"change your default search to Google"*) and it knows where its own settings
live, or suggests `bro config`.

---

## Talk to it: voice dictation

bro was built to be spoken to. Hold a key, say *"Bro, find the biggest files in my downloads
folder"*, let go, press Enter.

**We recommend [Handy](https://github.com/cjpais/Handy):** free, open source, and it transcribes
entirely on your Mac, so nothing you say leaves the machine.

1. Install Handy from its [GitHub releases](https://github.com/cjpais/Handy/releases).
2. Pick a shortcut in Handy, click into your terminal, hold the shortcut and say
   *"Bro, …"*.
3. The text lands on your command line. Press Enter and bro takes it from there.

[Wispr Flow](https://wisprflow.ai) and macOS's built-in Dictation work too.

Dictation tools add capitals, commas and full stops (`Bro, open Safari.`). bro handles all of it,
and the eval suite has spoken-style cases to keep it that way.

---

## Pick a model

`bro config` walks you through it, offers presets, and sends a test request at the end. bro talks
to anything with an OpenAI-compatible `/v1/chat/completions` endpoint.

| Preset | Where it runs | Cost | Speed | Notes |
|---|---|---|---|---|
| **Google Gemini** (`gemini-3.5-flash-lite`) | Google's cloud | free tier available | **~0.9 s** | Most accurate in our evals. Needs an API key from [Google AI Studio](https://aistudio.google.com/apikey). |
| **apfel** (Apple's on-device model) | your Mac | free | ~1.8 s | Private, no key needed, macOS 26+. [apfel](https://apfel.franzai.com/) serves Apple's built-in model; start it with `brew services start apfel`. Small model, small context: fine for everyday commands, weaker on anything unusual. |
| **Ollama** | your Mac | free | depends on model and Mac | e.g. `ollama pull qwen3:8b` |
| **llama.cpp server** | your Mac | free | depends | |
| **LM Studio** | your Mac | free | depends | |
| **Custom** | anywhere | — | — | Any OpenAI-compatible endpoint. |

**Using Gemini:**

```sh
echo 'export GEMINI_API_KEY="your-key"' >> ~/.zshrc   # bro stores the variable's name, never the key
exec zsh
bro config                                             # choose "Google Gemini (cloud)", press Enter for the defaults
```

> Some models are slow through Google's API. In our tests Gemma 4 took 24 s to start answering,
> and the free tier allows only 20 requests a day for some Flash models. Flash-Lite is fast and
> has a roomier free tier, which is why it's the default.

When the endpoint isn't on your Mac or local network, `bro config` warns you once that
(redacted) terminal context will be sent there.

---

## What bro looks at

Every source is gathered **in parallel**, each with a 30 ms time limit; a slow source is skipped,
never waited on. The total is capped at a token budget (6,000 by default, 2,500 for small
on-device models) and trimmed from the bottom of this list:

| Source | Details |
|---|---|
| Your request and the current line | exactly as typed or dictated |
| Your notes | `~/.config/bro/bro.md`, see [bro.md](#bromd-notes-for-bro) |
| Anything you pipe in | see [Pipe things into bro](#pipe-things-into-bro) |
| Your last 3 commands | with exit status, and their **output** when running inside [CTX](#ctx) |
| Current folder | path, git branch and short status, the first 50 entries |
| Open tabs | inside CTX: number, name, folder, what's running |
| Running processes | top 15 by CPU (averaged since each started) and by memory, plus processes you mentioned by name, plus your shell's jobs |
| Environment | variable names; values only for safe ones like `PATH` and `EDITOR` (see [Privacy](#privacy)) |
| Aliases and functions | names, so bro can use yours |
| Installed apps | and your default browser |
| Your projects | git repos and folders under `~`, most relevant first |
| What you ran before | past requests and the command you actually ran (last 20, most relevant first) |
| Saved tabs | inside CTX: past sessions matching your request ("how did I deploy last time") |
| Man pages | for commands you name, trimmed to the options that match your request, so the model uses *this* Mac's BSD flags |
| This Mac | macOS version, CPU architecture, shell, terminal |

Run `bro --show-prompt <request>` to see exactly what would be sent, without sending it.

---

## Safety

- **bro never executes anything.** Its only output is text in your prompt, or on stdout.
  There is no `--run` flag, on purpose.
- **Danger flag.** A local check, independent of the model, flags risky commands: `rm -r`,
  `sudo`, `dd`, `mkfs`, `chmod -R`, overwriting a file with `>`, `curl … | sh`, killing system
  processes, `git push --force`, `git reset --hard` and more. The explanation turns **red** and
  starts with ⚠. Nothing is blocked; you just can't miss it.
- **Nothing hidden.** Multi-line and chained (`;`, `&&`) commands are shown in full.

---

## Privacy

Your terminal is full of secrets, so **nothing key-like goes into the prompt, even for a local
model**. Redaction is always on and can't be turned off.

- **Environment variables:** names are sent; values only for an allowlist (`PATH`, `HOME`,
  `SHELL`, `LANG`, `EDITOR`, `TERM*`…). Anything named like `KEY`, `TOKEN`, `SECRET`, `PASS`,
  `AUTH`, `CRED`, `SESSION` or `COOKIE` never has its value sent.
- **Everything else** (commands, output, history, saved tabs) goes through
  [redact](https://github.com/Dissipation-AI/redact), which replaces with `[redacted]`:
  - known key formats (AWS, OpenAI, Anthropic, GitHub, GitLab, Slack, Stripe, JWTs…);
  - private-key blocks, `Authorization`/`Bearer` headers, passwords in URLs;
  - secret-looking assignments (`DB_PASSWORD=…`, `--token …`, `"api_key": …`);
  - any long, random-looking token, even in formats nobody has listed.
- Redaction runs **last, on the fully assembled prompt**, so no source can slip past it.
- Your learning history lives at `~/Library/Application Support/bro/history.jsonl` and is
  redacted too. Caches live in `~/Library/Caches/bro/`. Nothing is sent anywhere except to the
  model you configured.

---

## From scripts and other tools

bro is a small Unix tool: intent in, one command out.

```console
$ bro list the five biggest files here
du -h * | sort -hr | head -n 5               ← stdout
lists the five largest files in this folder  ← stderr

$ echo "list the five biggest files here" | bro --json
{"command":"du -h * | sort -hr | head -n 5","explanation":"…","confidence":"high","dangerous":false,"danger_reason":null}
```

With a request *and* piped input (`some-command | bro --json <request>`), the piped text is
context for the request; with `--json` it isn't echoed back.

Exit status is 0 when a command was produced, 1 when not. bro doesn't know who is calling; a
tool that wants a command for its *own* syntax puts that tool's help text in the request.

| Flag | |
|---|---|
| `--json` | answer as JSON: `command`, `explanation`, `confidence`, `dangerous` |
| `--show-prompt` | print what would be sent, and don't send it (with `--json`: the request body) |
| `--endpoint <url>`, `--model <name>` | override the configured model for one request |
| `--line <text>` | a half-typed command to finish |
| `--context-file <file>` | use a saved context snapshot instead of this Mac (used by the evals) |
| `--timings` | how long each step took, to stderr |

---

## Configuration

`bro config` writes `~/.config/bro/config.toml`. You can edit it by hand:

```toml
endpoint = "https://generativelanguage.googleapis.com/v1beta/openai"
model = "gemini-3.5-flash-lite"
api_key_env = "GEMINI_API_KEY"     # the NAME of an env var holding the key; never the key
timeout_ms = 15000
temperature = 0.0
max_context_tokens = 6000          # 2500 for apfel
search_url = "https://search.brave.com/search?q={query}"   # Brave, Google, DuckDuckGo, Kagi…
browser = "default"                # or an app name, e.g. "Firefox"
complete_key = "^G"                # the "finish this line" key
history_examples = 20              # past requests sent as examples
```

`bro config --check` sends a tiny request and tells you how long it took.

---

## bro.md: notes for bro

`~/.config/bro/bro.md` is yours: plain text that bro adds to its instructions on every request,
like `CLAUDE.md` for Claude Code. Put in whatever bro should know about you and this computer:

```markdown
- My GPU box is the host lab-gpu (ssh config is set up).
- I use uv, not pip, and pnpm, not npm.
- "The blog" means ~/code/blog. Deploy it with ./scripts/ship.
- Prefer ripgrep (rg) over grep.
```

Then `bro install requests` gives `uv add requests`, and `bro connect to the gpu box` gives
`ssh lab-gpu`.

You can also just tell bro: `bro remember that my staging server is 10.20.0.8` suggests
`echo '- my staging server is 10.20.0.8' >> ~/.config/bro/bro.md`. Press Enter and it knows from
then on. The notes are redacted like everything else, and long files are cut to fit the model's
budget.

---

## How it works

```
 you type or dictate              zsh widget (wraps Enter)                       bro (Rust, ~4 MB)
 "Bro, kill that ghostty"  ──▶   line starts with "bro"? ── no ──▶ normal Enter    │
                                         │ yes                                     │
                                         ▼                                         ▼
                                 runs bro with the words  ──────────▶  gather context in parallel
                                 verbatim (no zsh parsing)             (30 ms limit per source)
                                                                                   │
                                                                     build prompt, redact secrets
                                                                                   │
                                                                                   ▼
                                                                     your model (streamed reply)
                                                                                   │
  your prompt: kill 812█   ◀──── fills the line as it streams ◀──── parse, flag danger
  "stops the Ghostty process"
```

- **The trigger** is a ZLE widget wrapping `accept-line`, so natural language never reaches zsh's
  parser (`what's` would open a quote, `?` is a glob, `!` is history expansion).
- **The model** is asked for one line of JSON (`command`, `explanation`, `confidence`), enforced
  with a JSON schema where the server supports it. Small models get forgiving parsing: broken
  escapes, code fences, cut-off answers and bare commands are all recovered.
- **Streaming:** the command appears in your prompt as it's generated.
- **Learning:** after bro fills your prompt, the widget records what you actually ran and whether
  it worked. Similar future requests get those as examples.
- **Small and boring stack:** `ureq` + rustls (no async runtime), `gix` for git, `libproc` for
  processes (no `ps`), `lexopt` for flags. Every dependency is checked with `cargo deny` and
  `cargo audit`. CI enforces the budgets: binary under 5 MB, startup to request ready under 10 ms
  (about 3 ms in practice).

### CTX

bro works in any terminal. It works even better inside **CTX**, the AI-first terminal it was
built for: there it can also read your recent commands' **output** and your other tabs, so
"fix it", "why did that fail" and "show me the dev-server tab" have the full picture. bro talks to
CTX only through the `ctx` command line; without it, those sources are simply absent.

---

## Evals

`evals/run.sh` runs **80 realistic cases** (each a saved context snapshot plus the commands that
count as right or wrong) against any endpoint, and against other tools as baselines. 30 of them
are held out: they were never used while tuning prompts. Ten of those come from areas bro has no
examples for at all (data science, research, remote servers), to check it stays general.

```sh
evals/run.sh                                           # your configured model
evals/run.sh --endpoint http://localhost:11434/v1 --model apple-foundationmodel
evals/run.sh --pause 6 --model gemini-3.5-flash-lite   # --pause for rate-limited free tiers
evals/run.sh --tool butterfish                         # baselines: butterfish, apfel-cmd
evals/run.sh kill-ghostty bsd-sed                      # just some cases
```

Results on an M4 Pro, October 2026:

| Tool and model | All 80 cases | Held-out cases | Typical time per request |
|---|---|---|---|
| **bro + Gemini 3.5 Flash-Lite** | **76 / 80** | **30 / 30** | **0.85 s** |
| bro + apfel (Apple on-device) | 37 / 80 | 6 / 30 | 1.9 s |
| [Butterfish](https://github.com/bakks/butterfish) + apfel | — | 7 / 20 (the first 20) | — |
| apfel's own `cmd` | — | 7 / 20 (the first 20) | — |

The baselines only see the request text, which is the point: most misses there are cases that
need context ("fix it", "kill that", "open the notes").

Each run is appended to `evals/results/<tool>.tsv`, so you can compare over time.

**Nothing is hard-coded per task.** The prompt explains behaviour (use the context, fix from the
error output, look before you edit), and the ten built-in examples each demonstrate one of those
behaviours, not a command to recall. Rules and examples are chosen by what's in the context (a
failed command, open tabs, piped text), never by keywords in the request. Knowing commands is
the model's job; manual excerpts, your history and your `bro.md` fill the gaps.

---

## Development

```sh
cargo test                    # unit tests, a fake model server, and the zsh widget driven in a real PTY
cargo clippy --all-targets -- -D warnings
cargo deny check && cargo audit
cargo build --release && target/release/bro --timings --show-prompt list files
```

| Path | |
|---|---|
| `src/main.rs` | arguments, request, output |
| `src/context/` | one module per context source |
| `src/prompt.rs` | budgets, trimming and prompt assembly |
| `src/llm.rs` | OpenAI-compatible streaming client, forgiving parsing |
| `src/safety.rs` | danger flags, redaction |
| `shell/bro.zsh` | the zsh widget (embedded; printed by `bro init zsh`) |
| `prompts/system.md` | the system prompt (embedded at build time) |
| `evals/` | eval runner, cases and results |
| `SPEC.md` | the full design |

The zsh tests run each case with Ghostty's, iTerm2's and Terminal.app's own shell integration
loaded, to make sure bro coexists with all of them.

## Licence

[Apache-2.0](LICENSE)
