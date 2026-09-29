#!/bin/sh
set -eu
grep -q 'if got := o.Total(); got != 900' internal/orders/orders_test.go
go test ./...
