#!/bin/sh
set -eu
grep -qE 'export const CURRENCY' lib/config.ts
grep -qE 'export const LOCALE' lib/config.ts
! grep -qE 'export const (CURRENCY|LOCALE)' lib/price.ts
grep -q 'config' lib/price.ts
node --test lib/cart.test.ts
