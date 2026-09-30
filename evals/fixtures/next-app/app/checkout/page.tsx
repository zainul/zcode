import { CheckoutForm } from "../../components/CheckoutForm.tsx";
import { Cart } from "../../lib/cart.ts";

export default function CheckoutPage() {
  const cart = new Cart();
  return (
    <main>
      <h1>Checkout</h1>
      <CheckoutForm cart={cart} />
    </main>
  );
}
