#!/bin/sh
set -eu
[ "$(grep -c '^test(' lib/cart.test.ts)" -gt 2 ]
grep -q 'removeItem' lib/cart.test.ts
node --test lib/cart.test.ts
