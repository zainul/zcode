# 1. Installation

← [Guide index](README.md) · Next: [Configuration](02-configuration.md)

## Requirements

| | |
|---|---|
| Rust | **1.85.0** — pinned by `rust-toolchain.toml`, so `rustup` installs it automatically on the first build |
| Platforms | Linux x86_64 and macOS aarch64 are tested. Windows builds, but the TUI and PTY paths are untested there |
| Network | Only to reach your LLM provider (and to fetch crates on the first build) |

Nothing else: no Node, no Python, no system libraries. TLS is provided by
`rustls`, so there is no OpenSSL dependency.

## The quick way: the install script

From a clone of the repository:

```sh
$ ./scripts/install.sh
```

It detects your platform, builds the release binary, installs it somewhere on
your `PATH`, and tells you what to do if that directory is not on your `PATH`
yet:

```
==> Platform: macos (arm64)
  ok cargo 1.85.0
==> Destination: /usr/local/bin
==> Building (this takes a few minutes the first time)
  ok built 4.8M
  ok installed /usr/local/bin/zcode
  ok /usr/local/bin is on your PATH

==> Verifying
zcode v0.2.0 (git: 9a99381..., profile: release)
```

Where it installs, in order of preference:

1. `/usr/local/bin` — if it exists and you can write to it
2. `~/.local/bin` — otherwise, created if needed

Options:

```sh
$ ./scripts/install.sh --prefix ~/bin    # somewhere specific
$ ./scripts/install.sh --no-build        # install an existing target/release build
$ ./scripts/install.sh --help
$ ZCODE_INSTALL_DIR=~/bin ./scripts/install.sh   # same as --prefix
```

If `/usr/local/bin` needs root:

```sh
$ sudo ./scripts/install.sh --prefix /usr/local/bin
```

The script is POSIX `sh`, so it runs under bash, zsh, dash and ash. On Windows
use Git Bash, MSYS2, or WSL — it detects those and warns that the TUI is
untested there, while `zcode run` works fine.

## The manual way

```sh
$ git clone <this-repo>
$ cd zcode
$ cargo build --release
```

The first build downloads the pinned toolchain and compiles the dependency
tree; expect a few minutes. Later builds take seconds. The binary lands at
`target/release/zcode` and is self-contained:

```sh
$ ls -lh target/release/zcode
-rwxr-xr-x  1 you  staff   4.8M  zcode
```

Put it on your `PATH`:

```sh
$ sudo cp target/release/zcode /usr/local/bin/zcode      # system-wide
# or, without sudo:
$ mkdir -p ~/.local/bin && cp target/release/zcode ~/.local/bin/zcode
$ export PATH="$HOME/.local/bin:$PATH"                   # add to ~/.zshrc or ~/.bashrc
```

Or install straight from the source tree with cargo:

```sh
$ cargo install --path crates/cli
```

## Verify

```sh
$ zcode version
zcode v0.2.0 (git: 9a99381..., profile: release)
```

`zcode version` prints the build metadata compiled into the binary — the crate
version, the git commit it was built from, and the profile. Quote this line in
bug reports.

```sh
$ zcode --help
zcode runs coding tasks against an LLM with native file, shell, MCP and LSP tools.

Configure it with zcode.json or zcode.toml; API keys are read from the environment by the name given in `api_key_env`.

Usage: zcode [COMMAND]

Commands:
  version  Print the version, git commit, and build profile
  run      Run a single task and exit
  repl     Open the interactive TUI
  session  Create, resume, fork, import, or export saved sessions
  tools    Inspect the tool registry
  skills   List the markdown skills the agent can load
  help     Print this message or the help of the given subcommand(s)
```

## Confirm the tools are present

This works with no configuration and no API key, so it is a good check that the
binary is sound:

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

Seven native tools. MCP and LSP tools appear here too once you configure
servers — see [chapter 9](09-mcp-and-lsp.md).

## Building a leaner binary (optional)

MCP and LSP support are cargo features, on by default. Turn them off if you
want the smallest possible build:

```sh
$ cargo build --release --no-default-features
```

## Updating

```sh
$ git pull
$ ./scripts/update.sh
```

`update.sh` is a wrapper around `install.sh`; either works. The new binary
replaces the existing one **where it already lives**, so you never end up with
two copies shadowing each other on `PATH`, and it shows both build stamps so
you can confirm the update landed:

```
==> Updating the existing installation in /home/you/.local/bin
  was: zcode v0.2.0 (git: 9a99381, built: 2026-08-26T03:31:35Z, release)
  now: zcode v0.2.0 (git: 4c1f2a8, built: 2026-08-26T09:12:44Z, release)
  ok updated /home/you/.local/bin/zcode
```

If the two stamps are identical, you re-installed the same build — run
`git pull` and try again.

If another copy is found earlier on your `PATH`, the script says so and tells
you to remove it, because that copy is the one your shell will actually run.

### "My new subcommand is not there"

Almost always a stale binary. Check which one you are running and when it was
built:

```sh
$ command -v zcode
/home/you/.local/bin/zcode
$ zcode version
zcode v0.2.0 (git: 9a99381, built: 2026-08-26T03:31:35Z, release)
```

Compare the `built:` stamp with your working tree. The version number and
commit stay the same between releases, so the timestamp is the reliable
signal. `git pull && ./scripts/update.sh` fixes it.

Sessions written by an older build keep working: session files carry a
`version` field and are read back by id.

## Uninstalling

```sh
$ ./scripts/uninstall.sh
```

It finds every `zcode` on your `PATH` plus the usual install locations, shows
what it found, and asks before deleting anything:

```
==> Found:
  /usr/local/bin/zcode  (zcode v0.2.0 (git: 9a99381..., profile: release))
Remove 1 file(s)? [y/N] y
  ok removed /usr/local/bin/zcode

==> Uninstalled.
```

Options:

```sh
$ ./scripts/uninstall.sh --yes             # no prompt (required in scripts and CI)
$ ./scripts/uninstall.sh --prefix ~/bin    # only the copy in that directory
```

If the directory needs root, the script says so rather than failing silently:

```sh
$ sudo ./scripts/uninstall.sh --yes
```

**Your project data is left alone.** Configuration and state live inside your
projects, and the uninstaller will not search your filesystem for them. Remove
them yourself if you want them gone:

```sh
$ rm -rf .zcode zcode.json zcode.toml
$ find . -maxdepth 3 -name '.zcode' -o -maxdepth 3 -name 'zcode.json'
```

Your API key export and any `PATH` line you added at install time also remain
in your shell startup file.

If you installed with `cargo install`, remove it the same way:

```sh
$ cargo uninstall zcode
```

---

Next: [Configuration](02-configuration.md)
