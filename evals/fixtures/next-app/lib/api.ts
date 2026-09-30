import type { CartItem } from "./cart.ts";

export interface CheckoutResult {
  orderId: string;
  totalCents: number;
}

/** POST the cart to the backend and return the created order. */
export async function submitCheckout(items: readonly CartItem[], fetchImpl = fetch): Promise<CheckoutResult> {
  const res = await fetchImpl("/api/checkout", {
    method: "POST",
    headers: { "content-type": "application/json" },
    body: JSON.stringify({ items }),
  });
  if (!res.ok) {
    throw new Error(`checkout failed: ${res.status}`);
  }
  return (await res.json()) as CheckoutResult;
}
