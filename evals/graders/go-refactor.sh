#!/bin/sh
set -eu
ls internal/pricing/*.go >/dev/null
grep -rq 'func ApplyDiscount(cents int64, pct int) int64' internal/pricing
grep -rq 'pricing.ApplyDiscount' internal/orders
go build ./...
go vet ./...
