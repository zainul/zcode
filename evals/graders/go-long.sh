#!/bin/sh
set -eu
grep -rq 'StatusCancelled' internal/orders
grep -rq 'ErrAlreadyShipped' internal/orders
grep -rq 'func (s \*Service) Cancel(id string) (Order, error)' internal/orders
grep -rqi 'cancel' internal/httpapi/handler_test.go
go vet ./...
go test ./internal/httpapi/ ./internal/orders/ -run 'Cancel'
# The planted discount bug is out of scope; everything else must pass.
go test ./internal/httpapi/
