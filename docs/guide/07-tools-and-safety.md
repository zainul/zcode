# 7. Tools & safety

← [Sessions](06-sessions.md) · [Index](README.md) · Next: [Agent modes](08-agent-modes.md)

Tools are what turn a chat model into an agent. `zcode tools list` shows exactly
what the model can reach:

```sh
$ zcode tools list
read                         Read a UTF-8 text file and return its full contents.
write                        Create or overwrite a file with the given contents (atomic).
str_replace_editor           Edit files in place. `view` shows a file, `create` writes one, ...
apply_patch                  Apply a unified diff to the working tree. Supports multiple files, ...
list_dir                     List the entries of a directory (directories end with `/`).
shell                        Run a shell command. Only commands permitted by the configured allowlist ...
zcode_skill                  Load a markdown skill from the configured skills directory as extra context.
```

Every file edit happens **in-process**. The agent never shells out to `sed` or
`awk`, so edits behave identically on every platform and are not subject to the
shell allowlist.

## The native tools

| Tool | Arguments | Notes |
|------|-----------|-------|
| `read` | `path` | Whole file as text |
| `write` | `path`, `content` | Creates parent directories; atomic (temp + rename) |
| `str_replace_editor` | `command`, `path`, plus `old_str`/`new_str`/`file_text` | `command` is `view`, `create`, `str_replace`, or `list_dir` |
| `apply_patch` | `patch` | A unified diff, possibly spanning several files |
| `list_dir` | `path` | Directories are suffixed with `/` |
| `shell` | `command`, optional `cwd`, `timeout_ms` | Allowlisted — see below |
| `zcode_skill` | `name` | Loads a skill; the tool description lists what exists |

`str_replace` requires `old_str` to appear in the file; the first occurrence is
replaced. If it appears more than once the result says so
(`edited src/main.rs (first of 3 occurrences)`) so the model can disambiguate.
If it is absent, the tool reports:

```
`old_str` not found in src/main.rs — read the file first and copy the exact text
```

That is a *tool error*, not a run failure: it is fed back to the model, which
usually re-reads and retries. Only infrastructure failures abort a run.

## Patches

`apply_patch` is the efficient path for multi-file edits:

```
--- a/src/main.rs
+++ b/src/main.rs
@@ -3,5 +3,9 @@
 }

 fn greet(name: &str) -> String {
     format!("Hello, {}!", name)
 }
+
+/// Returns a farewell for `name`.
+fn farewell(name: &str) -> String {
+    format!("Goodbye, {}!", name)
+}
```

Three properties worth knowing:

- **Line numbers are advisory.** Hunks are located by matching their context,
  so a diff generated against a slightly stale copy still applies.
- **All-or-nothing.** Every hunk across every file is computed before anything
  is written. A patch that fails on its third file leaves the first two
  untouched.
- **Create and delete work.** `--- /dev/null` creates a file; `+++ /dev/null`
  deletes one.

A patch that will not fit says so precisely, and the run continues:

```
  apply_patch: error: hunk 2 does not match src/lib.rs — re-read the file and
  rebuild the diff from its current contents
```

## Shell safety

Arbitrary shell access is how coding agents cause real damage. zcode applies
three checks, in order, before a command reaches `sh -c`.

### 1. Structure

Substitution and unrestricted redirection are refused outright — `` ` ``,
`$(`, `${`, `>`, `<`, `&` — because `echo hi $(rm -rf /)` would otherwise
satisfy an `echo .*` rule.

Three redirections are provably safe and are stripped before this check, so
they work:

```sh
go build ./... 2>&1          # duplicate stderr onto stdout
go test ./... >/dev/null     # discard
make -s build >/dev/null 2>&1
```

Nothing else does. `echo hi > /etc/passwd 2>&1` is still refused: the safe
suffix is stripped, and what remains still contains a `>`.

**This check is skipped when the allowlist is unrestricted.** If any pattern in
`shell_allowed` already matches every possible command — `".*"` being the one
people write — there is nothing left to smuggle past, and

```sh
cd /workspace && go build ./... 2>&1 | head
```

runs. Structure exists to stop a *narrow* pattern being widened by text the
shell expands later; once you have allowed everything, checking the text a
second time only produces refusals you did not ask for. The denylist below is
not skipped.

### 2. The denylist — which `shell_allowed` cannot override

A short list of commands is refused regardless of configuration: the ones with
no undo, the ones that escalate, the ones that pipe the network into a shell,
and the ones that publish irreversibly.

| Category | Examples |
|----------|----------|
| Irreversible destruction | `rm /`, `dd … of=`, `mkfs`, `shred`, fork bombs |
| Privilege escalation | `sudo`, `doas`, `su`, `chmod 777`, `chown root` |
| Fetch-and-run | `curl … \| sh`, `wget … \| bash` |
| Host state | `shutdown`, `reboot`, `halt`, `killall` |
| Irreversible publication | `git push --force`, `git reset --hard`, `git clean -f`, `npm publish`, `cargo publish` |
| Credential exfiltration | `~/.ssh/id_*`, `~/.aws/credentials` |

Even with `"shell_allowed": [".*"]`, these are refused, and the error says so
explicitly so the model does not waste a turn trying to widen the allowlist:

```
shell: error: command refused: it matches zcode's built-in denylist
(\brm\s+(-[a-zA-Z]*\s+)*-[a-zA-Z]*[rR]), which `shell_allowed` cannot
override: sudo rm -rf /tmp/zcode-test
```

#### Recursive deletes are judged by their target

`rm -rf` is not banned outright. A rule that reads the flag and never the path
refuses `rm -rf node_modules` exactly as hard as `rm -rf /` — which is not
safety, it is just a reason to switch the guard off. So the target is what is
checked, and the bar is *can a reader tell what will be gone afterwards*:

| Command | |
|---------|---|
| `rm -rf node_modules` | ✔ a specific directory, inside the working tree |
| `rm -rf ./target`, `rm -rf dist/` | ✔ |
| `rm -rf src/generated` | ✔ — deleting your own source is your business |
| `rm -rf /tmp/build-123` | ✔ scratch space, naming something *in* it |
| `rm -rf /` | ✖ |
| `rm -rf ~`, `rm -rf ~/Documents` | ✖ the home directory |
| `rm -rf *`, `rm -rf build/*` | ✖ a glob the shell expands after the check |
| `rm -rf $BUILD_DIR` | ✖ likewise a variable |
| `rm -rf .`, `rm -rf ..`, `rm -rf ../x` | ✖ the tree itself, or out of it |
| `rm -rf /usr`, `rm -rf /etc/nginx` | ✖ absolute, outside scratch |
| `rm -rf /tmp` | ✖ the scratch root itself |
| `rm -rf` | ✖ no target at all |

The refusal names the offending word, not just the line, so the model can fix
it rather than retry it:

```
command refused: `rm -r` must name a specific path, and "~/Library" does not —
globs, `~`, `..`, `$VAR` and absolute paths outside /tmp are refused because
what they delete cannot be read from the command: rm -rf build dist ~/Library
  hint: name the directory itself, e.g. `rm -rf node_modules` or `rm -rf ./target`
```

It cannot be dodged by hiding the `rm`: `cd /tmp && rm -rf /`,
`find . -exec rm -rf {} +` and `ls | xargs rm -rf /` are all refused, and so is
`/bin/rm`. Quoting does not help either — `rm -rf "$HOME"` is the same request.

If you want no recursive deletes at all, `shell_denied` still gets you there:

```json
{ "shell_denied": ["\\brm\\s+.*-[a-zA-Z]*[rR]"] }
```

Add your own rules with `shell_denied`. They *extend* the built-ins — a project
file cannot remove a rule set machine-wide:

```json
{ "shell_denied": ["\\bterraform apply\\b", "\\bkubectl delete\\b"] }
```

### 3. The allowlist

Every segment of the command must match a pattern in `shell_allowed` **in
full**. The command is split on `;`, `|`, and newlines, and each part is checked
independently, so `echo hi; rm -rf /` fails on its second half.

The default allowlist covers the toolchains a coding agent actually needs:

| Group | Covered |
|-------|---------|
| Inspect | `ls`, `cat`, `head`, `tail`, `wc`, `grep`, `rg`, `find`, `fd`, `tree`, `diff`, `sed`, `awk`, `jq`, `stat`, `du`, … |
| Files | `mkdir`, `touch`, `cp`, `mv`, `ln`, `rm` (recursive too, for a specific path — see above) |
| Version control | `git`, `gh`, `glab` |
| Rust | `cargo`, `rustc`, `rustup`, `rustfmt` |
| Go | `go`, `gofmt`, `goimports`, `golangci-lint`, `staticcheck`, `dlv`, `air`, `templ` |
| Node / TS / Next.js | `node`, `npm`, `npx`, `pnpm`, `yarn`, `bun`, `deno`, `tsc`, `eslint`, `prettier`, `vitest`, `jest`, `next`, `vite`, `turbo` |
| Python | `python`, `pip`, `uv`, `poetry`, `pytest`, `ruff`, `mypy`, `black` |
| Build | `make`, `just`, `task`, `cmake`, `ninja`, `bazel`, `gradle`, `mvn`, `dotnet` |
| Containers (read-mostly) | `docker ps/images/logs/inspect/compose`, `kubectl get/describe/logs`, `terraform validate/fmt/plan` |

This is deliberately generous. The old default was `echo`, `ls`, `cd`, `cat`,
which meant `go build ./...` failed on a fresh install — and the fix everyone
reached for was `"shell_allowed": ["*"]`, switching the safety net off
entirely. A workable default paired with a denylist is strictly safer than a
useless default people disable.

It is still an allowlist: `ssh`, `scp`, `nc`, `systemctl`, `crontab` are not in
it. `kubectl delete` and `terraform apply` are not either.

### Configuring it

```json
{
  "shell_allowed": ["echo .*", "go( .*)?", "git (status|diff|log)( .*)?"],
  "shell_denied": ["\\bgo run\\b"]
}
```

Rules worth knowing:

1. **An empty list blocks everything.** Omitting the key keeps the defaults
   above. To lock the agent out of the shell entirely, set
   `"shell_allowed": []` explicitly — or just use `--mode editing`, which
   withholds the tool.
2. **Patterns are anchored.** `ls .*` must match a whole segment; `sudo ls /`
   does not match it.
3. **`".*"` means everything.** A pattern that matches any command switches off
   the structure check as well, so `&&`, pipes and redirection all work. The
   denylist stays on — this widens what may run, it never removes a built-in
   rule. `zcode config` says so plainly, so the state is never a surprise:

   ```
   shell_allowed          1 pattern(s) — unrestricted: anything the denylist
                                         permits, pipes and `&&` included
   shell_denied           23 built-in + 0 from config
   ```
4. **These are regular expressions, not shell globs.** `*` alone is not a valid
   regex and makes every run fail; `.*` is what matches any text. An invalid
   pattern is reported with a hint:

   ```
   invalid shell_allowed pattern "*": error: repetition operator missing expression
     hint: these are regular expressions, not shell globs — use ".*" to allow
           every command (which disables the safety net entirely)
   ```

   `zcode config` catches this before you spend a token on it, and exits 1.
5. **A block explains itself.** The model is told what to do about it, so it
   fixes the command instead of retrying it verbatim:

   ```
   shell: error: command blocked by the shell allowlist (`shell_allowed` in
   zcode.json/zcode.toml): go build ./...
     hint: no pattern in `shell_allowed` matches `go`; add one, e.g. "go( .*)?"
   ```

   A command refused for its *structure* is pointed at the escape hatch
   instead:

   ```
   shell: error: command blocked by the shell allowlist (`shell_allowed` in
   zcode.json/zcode.toml): cd /workspace && go build ./...
     hint: shell metacharacters (`$(`, backticks, `>`, `<`, `&&`) are not
           allowed under a narrow allowlist; only `2>&1` and `>/dev/null` are.
           Run the command without them, or set `shell_allowed` to [".*"],
           which permits every command the built-in denylist does not refuse.
   ```

   In the TUI that message is wrapped in full underneath its tool row, never
   truncated — a refusal you cannot read is a refusal you cannot fix.

## Token-optimised shell output (rtk)

Every byte a shell command returns is fed into the transcript and billed on
every subsequent turn. [rtk](https://github.com/rtk-ai/rtk) is a CLI proxy that
runs the command you asked for and filters what comes back:

| Command | Bare | Through rtk |
|---------|------|-------------|
| `ls -la` | 438 B | 61 B (−87%) |
| `git status` | 234 B | 98 B (−59%) |

Measured through zcode's own shell tool, on a small repository; the ratio grows
with the output.

**It is on by default when rtk is installed, and zcode will install it if it is
not.** Nothing needs configuring.

### What zcode does, and does not do

zcode reimplements none of rtk's filtering. Before running a shell command it
asks rtk one question — `rtk rewrite "<command>"` — which rtk documents as the
single source of truth its own hooks use. If rtk answers, that is what runs.

Deciding here which commands are safe to rewrite would mean duplicating rtk's
judgement and getting it wrong. `test -f x` must not become `rtk test -f x`
(different `test` entirely), `read` is a shell builtin, and `env FOO=1 make`
has to keep its prefix. rtk already knows all of this, so it is asked rather
than second-guessed.

### The guard runs first

The rewrite happens **after** the allowlist and denylist have passed the
original command. Both are written against the commands a person types: a
pattern like `git (status|diff)( .*)?` would stop matching the moment every
command grew an `rtk ` prefix, and an allowlist that refuses everything it was
written to permit is worse than no rtk at all.

The denylist therefore judges what was *meant* — `git push --force` is refused
before rtk ever sees it. The rewritten command is re-checked against the
denylist anyway, and discarded in favour of the original if it somehow trips
it. rtk only prepends a proxy, so that should never happen; "should never" is
not a property worth assuming about a command line assembled by another
program.

### Installing

| Situation | What happens |
|-----------|--------------|
| rtk on `PATH` | used, silently |
| not installed, Homebrew present | `brew install rtk` — announced first, then used |
| not installed, no Homebrew but Cargo present | `cargo install --git https://github.com/rtk-ai/rtk rtk` — announced first, then used |
| not installed, no supported package manager | a warning naming the command to run; zcode continues without it |
| an install failed | not retried for 24 hours |
| `rtk.enabled = false` | never looked for |

Detection costs nothing measurable and happens once per process. The install
happens at most once: it says what it is doing *before* it runs, because
`brew install` (or a from-source `cargo install`) can take a minute or more
and a first run that stalls without explanation reads as a hang. If it fails,
the failure is recorded machine-wide (`~/.config/zcode/rtk-install-failed`)
and not retried for a day — otherwise a machine with no network would pay the
package manager's failure cost on every single invocation, forever. A later
success clears the record.

Two installers, tried in order, and never more than that. `brew` is tried
first: `rtk` is in **homebrew-core** rather than a third-party tap, so what it
installs is a reviewed, auditable formula, and it also covers Linux machines
that have Linuxbrew. `cargo install --git https://github.com/rtk-ai/rtk` is
the fallback for the much more common Linux/Ubuntu case — no Homebrew, but a
Rust toolchain, since zcode itself needs one to build. It must be `--git`
pointed at the upstream repository, never plain `cargo install rtk`: that name
on crates.io resolves to **an unrelated crate** (Rust Type Kit, not the token
killer). `--git` compiles straight from the pinned upstream source rather than
fetching and running an installer script — upstream's own shell installer is
`curl … | sh`, the exact pattern
[zcode's own denylist refuses](#2-the-denylist--which-shell_allowed-cannot-override),
and a tool that forbids the model from piping the network into a shell has no
business doing it itself.

Installation only ever runs a package manager that is **already present**. It
installs a package; it does not install a package manager, and it never
downloads a script to execute.

### Configuring it

```json
{
  "rtk": {
    "enabled": true,
    "auto_install": true,
    "path": "/opt/homebrew/bin/rtk"
  }
}
```

| Key | Default | Meaning |
|-----|---------|---------|
| `enabled` | `true` | Use rtk when available. `false` stops zcode looking for it |
| `auto_install` | `true` | Install rtk when missing, via an existing package manager |
| `path` | *(none)* | An explicit binary, for a machine where rtk is not on `PATH` |

`ZCODE_RTK=0`, `ZCODE_RTK_AUTO_INSTALL=0` and `ZCODE_RTK_PATH` override these,
for turning it off in one shell or one CI job without editing a file.

`zcode config` always says which state you are in:

```
rtk                    0.36.0 — shell output is token-optimised  [/opt/homebrew/bin/rtk]
```

zcode is quiet when rtk works and loud when it does not: a line on every launch
about an optimisation that always works is noise, but a configured `path` that
does not resolve, or an install that fails, is a warning you will see.

## Skills

Skills are markdown notes the agent pulls in on demand — house style, review
checklists, domain background.

### Where they live

Three directories are searched, nearest first:

1. `<project>/.zcode/skills/` — the project's own
2. `skills_dir` from your config, if set
3. `~/.config/zcode/skills/` — your machine-wide library

A name found in an earlier directory shadows the same name later, so a project
can override a shared skill. Setting `skills_dir` **adds** a root rather than
replacing the project's, so a global library never hides per-project notes.

### Two layouts

```
skills/rust-style.md              a plain file
skills/rust-style/SKILL.md        a directory (the Agent Skills convention)
```

Both work. `README.md` and dot-files are ignored.

### Writing one

The first line of prose becomes the summary the model sees. Better: give it
YAML front matter, and the `description` is used instead.

```sh
$ mkdir -p .zcode/skills
$ cat > .zcode/skills/rust-style.md <<'EOF'
---
description: House Rust conventions for this repository.
---
# Rust style
- Every public fn carries a `///` doc comment.
EOF
```

### Checking what the agent can see

```sh
$ zcode skills list
Searched
  /home/you/project/.zcode/skills
  /home/you/.config/zcode/skills

2 skill(s) offered to the model
  review-checklist   How we review code in this repo.
  rust-style         House Rust conventions for this repository.
```

### How they are triggered

**The model decides.** Skills are not injected automatically — the agent is
given a `zcode_skill` tool whose description lists every available skill with
its summary, and calls it when one looks relevant:

```
Load a skill: project-specific guidance, conventions or checklists, as markdown.
Call this before starting work when one is relevant, and follow what it says.
Available skills:
- review-checklist: How we review code in this repo.
- rust-style: House Rust conventions for this repository.
```

Because the names and summaries are in the description, the model can choose
one unprompted. You can also ask directly:

```sh
$ zcode run "apply the rust-style skill to src/lib.rs"
```

If no skills exist anywhere, the tool is not offered at all — no wasted prompt
budget, and no invitation to guess names. Skills are read-only, so they are
available in planning mode too.

Naming is the whole trigger mechanism: a skill called `rust-style` with a clear
description gets picked up for Rust work; one called `notes` will not.

## Tool output limits

Results are truncated to `max_tool_output_chars` (default 32000) *before* they
enter the transcript, with `...[truncated]` appended. A `cat` of a huge file
cannot blow up the context window or your token bill.

## Adding your own tool

Implement `domain::Tool` (a `spec()` describing the JSON schema and a `call()`),
register it in `ToolRegistry::from_config`, and — if it modifies anything — add
its name to `domain::modes::execute_only_tool_names` so planning mode gates it.
The engine needs no changes.

---

Next: [Agent modes](08-agent-modes.md)
