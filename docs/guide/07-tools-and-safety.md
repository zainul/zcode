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
| Irreversible destruction | `rm -rf`, `rm -r`, `dd … of=`, `mkfs`, `shred`, fork bombs |
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
| Files | `mkdir`, `touch`, `cp`, `mv`, `ln`, `rm` (non-recursive; the denylist stops the rest) |
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

Results are truncated to `max_tool_output_chars` (default 16000) *before* they
enter the transcript, with `...[truncated]` appended. A `cat` of a huge file
cannot blow up the context window or your token bill.

## Adding your own tool

Implement `domain::Tool` (a `spec()` describing the JSON schema and a `call()`),
register it in `ToolRegistry::from_config`, and — if it modifies anything — add
its name to `domain::modes::execute_only_tool_names` so planning mode gates it.
The engine needs no changes.

---

Next: [Agent modes](08-agent-modes.md)
