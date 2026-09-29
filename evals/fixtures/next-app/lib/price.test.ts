import { test } from "node:test";
import assert from "node:assert/strict";
import { formatPrice, taxFor } from "./price.ts";

test("formatPrice keeps the cents", () => {
  assert.equal(formatPrice(1999), "$19.99");
});

test("formatPrice of zero", () => {
  assert.equal(formatPrice(0), "$0.00");
});

test("taxFor rounds to the nearest cent", () => {
  assert.equal(taxFor(1005, 8), 80);
});
