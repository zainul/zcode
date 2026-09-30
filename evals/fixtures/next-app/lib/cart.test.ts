import { test } from "node:test";
import assert from "node:assert/strict";
import { Cart } from "./cart.ts";

test("adding the same sku twice merges quantities", () => {
  const cart = new Cart();
  cart.addItem({ sku: "a", name: "A", unitCents: 500, quantity: 1 });
  cart.addItem({ sku: "a", name: "A", unitCents: 500, quantity: 2 });
  assert.equal(cart.lines().length, 1);
  assert.equal(cart.subtotal(), 1500);
});

test("total adds 8% tax", () => {
  const cart = new Cart();
  cart.addItem({ sku: "a", name: "A", unitCents: 1000, quantity: 1 });
  assert.equal(cart.total(), 1080);
});
