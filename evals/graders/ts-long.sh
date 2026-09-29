#!/bin/sh
set -eu
test -f lib/discount.test.ts
node --test lib/discount.test.ts lib/cart.test.ts
cat > .grader-check.test.ts <<'TS'
import { test } from "node:test";
import assert from "node:assert/strict";
import { applyDiscount } from "./lib/discount.ts";
test("grader: rules", () => {
  assert.equal(applyDiscount(1000, "SAVE10"), 900);
  assert.equal(applyDiscount(1006, "SAVE10"), 905);
  assert.equal(applyDiscount(5000, "FREESHIP"), 4500);
  assert.equal(applyDiscount(4999, "FREESHIP"), 4999);
  assert.equal(applyDiscount(1234, "NOPE"), 1234);
});
TS
node --test .grader-check.test.ts
