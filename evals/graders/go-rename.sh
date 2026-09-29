#!/bin/sh
set -eu
! grep -rnE 'Get\(id string\)' internal
grep -rqE 'Find\(id string\)' internal/orders/store.go
go build ./...
go vet ./...
go test ./internal/httpapi/
