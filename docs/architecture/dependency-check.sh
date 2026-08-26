#!/usr/bin/env bash
# Validates that the crate dependency graph matches the architectural topology
# defined in docs/prd/based-system/technical-plan.md §3.
#
# Usage: bash docs/architecture/dependency-check.sh
#
# The expected graph is:
#   cli ──► app ──► domain
#   cli ──► tools ──► infra/{filesystem,shell,config,mcp,lsp} ──► domain
#   cli ──► infra/{llm,mcp,lsp,session,telemetry,config} ──► domain
#
# Violations:
#   - domain depends on any third-party crate
#   - app depends on infra, tools, or cli (or any third-party crate but thiserror)
#   - infra/tools depend on cli/app

set -euo pipefail

cd "$(dirname "$0")/../.."

STATUS=0

INFRA_CRATES=(
    infra-llm infra-filesystem infra-shell infra-config
    infra-mcp infra-lsp infra-session infra-telemetry
)

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
APP_TREE=$(cargo tree -p app --depth 1 2>&1)
# `app` may reach only `domain` and `thiserror`; anything else is a violation.
UNEXPECTED=$(echo "$APP_TREE" | tail -n +2 | grep -oE '[a-z0-9_-]+ v[0-9]' \
    | awk '{print $1}' | grep -vE '^(domain|thiserror)$' || true)
if [ -n "$UNEXPECTED" ]; then
    echo "FAIL: app has unexpected direct dependencies:"
    echo "$UNEXPECTED"
    STATUS=1
else
    echo "OK: app depends only on domain + thiserror"
fi

echo ""
echo "=== Checking infra/tools acyclicity (FR-DI-03) ==="
for crate in "${INFRA_CRATES[@]}" tools; do
    CRATE_TREE=$(cargo tree -p "$crate" 2>&1)
    # Match "ag v" or "app v" as a package entry in the tree.
    if echo "$CRATE_TREE" | grep -Eqe '(^| )zcode v[0-9]|(^|\s)app v[0-9]'; then
        echo "FAIL: $crate depends on cli/app"
        STATUS=1
    else
        echo "OK: $crate has no upward dependency on cli/app"
    fi
done

echo ""
echo "=== Checking CLI composition root (FR-DI-04) ==="
CLI_TREE=$(cargo tree -p zcode 2>&1)
for dep in domain app tools "${INFRA_CRATES[@]}"; do
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
