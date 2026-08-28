The same setup as `../blocked-shell`, but with `"shell_allowed": [".*"]`.

`cd /tmp && printf … | tail -1` runs here. It used to be refused: the structure
check — which exists to stop `echo hi $(rm -rf /)` satisfying `echo .*` — ran
ahead of the allowlist and unconditionally, so an allowlist that permitted every
command still could not run one containing `&&` or a pipe.

The denylist is not skipped. `rm -rf /` is refused here exactly as everywhere.
