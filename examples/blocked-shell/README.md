A project whose `shell_allowed` names one command and nothing else, pointed at
the fake provider so the agent reliably attempts `cd … && go build`. It exists
to capture what a refusal looks like on screen: the message wraps in full under
its tool row rather than ending in an ellipsis.

Its sibling `../open-shell` is the same project with `"shell_allowed": [".*"]`,
where the same command runs.
