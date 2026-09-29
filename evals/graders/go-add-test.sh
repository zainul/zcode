#!/bin/sh
set -eu
[ "$(grep -c '^func Test' internal/httpapi/handler_test.go)" -gt 2 ]
grep -qE 'StatusNotFound|404' internal/httpapi/handler_test.go
go test ./internal/httpapi/
