#!/usr/bin/env bash
# Validates that the crate dependency graph matches the architectural topology
# defined in docs/prd/initial-scaffolding/technical-plan.md §3.
#
# Usage: bash docs/architecture/dependency-check.sh
#
# The expected graph is:
#   cli ──► app ──► domain
#   cli ──► infra/{llm,filesystem,shell,config} ──► domain
#
# Violations:
#   - domain depends on any third-party crate
#   - app depends on infra or cli
#   - infra depends on cli/app

set -euo pipefail

cd "$(dirname "$0")/../.."

STATUS=0

echo "=== Checking domain purity (FR-DI-01) ==="
# A pure-stdlib crate tree has exactly one line (the crate itself).
DOMAIN_LINES=$(cargo tree -p domain 2>&1 | wc -l | tr -d ' ')
if [ "$DOMAIN_LINES" -eq 1 ]; then
    echo "OK: domain is dependency-free ($DOMAIN_LINES line)"
else
    echo "FAIL: domain has third-party dependencies ($DOMAIN_LINES lines)"
    cargo tree -p domain 2>&1
    STATUS=1
fi

echo ""
echo "=== Checking app dependencies (FR-DI-02) ==="
APP_TREE=$(cargo tree -p app 2>&1)
# Match "ag v" or "app v" as a package entry (name followed by version),
# not as a substring of a file path.
if echo "$APP_TREE" | grep -Eqe '(^| )ag v[0-9]|(^|\s)(infra-llm|infra-filesystem|infra-shell|infra-config) v[0-9]'; then
    echo "FAIL: app depends on cli/infra crates"
    STATUS=1
else
    echo "OK: app depends only on domain"
fi

echo ""
echo "=== Checking infra acyclicity (FR-DI-03) ==="
for crate in infra-llm infra-filesystem infra-shell infra-config; do
    INFRA_TREE=$(cargo tree -p "$crate" 2>&1)
    # Match "ag v" or "app v" as a package entry in the tree.
    if echo "$INFRA_TREE" | grep -Eqe '(^| )ag v[0-9]|(^|\s)app v[0-9]'; then
        echo "FAIL: $crate depends on cli/app"
        STATUS=1
    else
        echo "OK: $crate has no upward dependency on cli/app"
    fi
done

echo ""
echo "=== Checking CLI composition root (FR-DI-04) ==="
CLI_TREE=$(cargo tree -p ag 2>&1)
for dep in domain app infra-llm infra-filesystem infra-shell infra-config; do
    if echo "$CLI_TREE" | grep -q "${dep} v"; then
        echo "OK: cli depends on $dep"
    else
        echo "FAIL: cli missing dependency on $dep"
        STATUS=1
    fi
done

echo ""
if [ "$STATUS" -eq 0 ]; then
    echo "All dependency checks passed."
else
    echo "Dependency violations detected."
fi

exit "$STATUS"
