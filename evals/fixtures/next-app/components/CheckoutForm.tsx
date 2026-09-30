"use client";

import { useState } from "react";
import { Cart } from "../lib/cart.ts";
import { formatPrice } from "../lib/price.ts";
import { submitCheckout } from "../lib/api.ts";

export function CheckoutForm({ cart }: { cart: Cart }) {
  const [status, setStatus] = useState<"idle" | "error" | "done">("idle");

  async function onSubmit(event: React.FormEvent) {
    event.preventDefault();
    try {
      await submitCheckout(cart.lines());
      setStatus("done");
    } catch {
      setStatus("error");
    }
  }

  return (
    <form onSubmit={onSubmit}>
      <p>Total: {formatPrice(cart.total())}</p>
      <button type="submit">Place order</button>
      {status === "error" && <p role="alert">Checkout failed, please retry.</p>}
      {status === "done" && <p>Thanks for your order!</p>}
    </form>
  );
}
