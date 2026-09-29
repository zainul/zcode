#!/bin/sh
set -eu
! grep -rn 'addItem' lib components app
grep -q '  add(item: CartItem): void' lib/cart.ts
node --test lib/cart.test.ts
