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

## The shell allowlist

Arbitrary shell access is how coding agents cause real damage, so `shell` is
default-deny. Only commands matching `shell_allowed` run:

```json
{ "shell_allowed": ["echo .*", "ls .*", "cargo (build|test|check).*", "git status"] }
```

The rules:

1. **An empty list blocks everything.** Omitting the key is different: you
   keep the built-in defaults (`echo .*`, `ls .*`, `cd .*`, `cat .*`). To lock
   the agent out of the shell entirely, set `"shell_allowed": []` explicitly.
2. **Patterns are anchored.** `ls .*` must match a whole command segment;
   `sudo ls /` does not match.
3. **Every segment must pass.** The command is split on `;` and `|`, and each
   part is checked independently — `echo hi; rm -rf /` is refused because of
   the second half.
4. **Substitution and redirection are refused outright.** Any command
   containing `` ` ``, `$(`, `${`, `>`, `<` or `&` is blocked, because
   `echo hi $(rm -rf /)` would otherwise satisfy an `echo .*` rule.
5. **These are regular expressions, not shell globs.** `*` on its own is not a
   valid regex and makes every run fail; `.*` is what matches any text. An
   invalid pattern is reported with a hint:

   ```
   invalid shell_allowed pattern "*": error: repetition operator missing expression
     hint: these are regular expressions, not shell globs — use ".*" to allow
           every command (which disables the safety net entirely)
   ```

   `zcode config` catches this before you spend a token on it.

In practice:

```
· shell
  shell: main.rs
· shell
  shell: error: command blocked by the shell allowlist (`shell_allowed` in zcode.json/zcode.toml): rm -rf /
Done.
```

The first command was allowed and ran; the second was refused, the refusal was
handed back to the model, and it carried on. Blocking a command never crashes a
run.

Start narrow and widen as you learn what your workflow needs. `cargo test.*` is
usually safe; `rm .*` rarely is.

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
