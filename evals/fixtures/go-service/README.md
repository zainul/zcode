# orders

A small order service used as an evaluation fixture for zcode. `go test ./...`
fails on purpose in `internal/orders` (`TestTotalAppliesDiscountOnce`): the
single-file-fix task asks the agent to find and fix it.
