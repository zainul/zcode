import { taxFor } from "./price.ts";

export interface CartItem {
  sku: string;
  name: string;
  unitCents: number;
  quantity: number;
}

export const TAX_RATE_PCT = 8;

/** A shopping cart. Amounts are integer cents. */
export class Cart {
  private items: CartItem[] = [];

  addItem(item: CartItem): void {
    const existing = this.items.find((i) => i.sku === item.sku);
    if (existing) {
      existing.quantity += item.quantity;
    } else {
      this.items.push({ ...item });
    }
  }

  removeItem(sku: string): boolean {
    const before = this.items.length;
    this.items = this.items.filter((i) => i.sku !== sku);
    return this.items.length < before;
  }

  lines(): readonly CartItem[] {
    return this.items;
  }

  subtotal(): number {
    return this.items.reduce((sum, i) => sum + i.unitCents * i.quantity, 0);
  }

  /** Subtotal plus tax at TAX_RATE_PCT. */
  total(): number {
    const subtotal = this.subtotal();
    return subtotal + taxFor(subtotal, TAX_RATE_PCT);
  }
}
