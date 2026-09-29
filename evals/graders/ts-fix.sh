#!/bin/sh
set -eu
grep -q 'assert.equal(formatPrice(1999), "\$19.99");' lib/price.test.ts
node --test
