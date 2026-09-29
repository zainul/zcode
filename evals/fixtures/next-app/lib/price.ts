export const CURRENCY = "USD";
export const LOCALE = "en-US";

/** Format an amount in cents as a currency string, e.g. 1999 → "$19.99". */
export function formatPrice(cents: number): string {
  const dollars = Math.floor(cents / 100);
  return new Intl.NumberFormat(LOCALE, { style: "currency", currency: CURRENCY }).format(dollars);
}

/** Tax on an amount in cents at `ratePct` percent, rounded to the nearest cent. */
export function taxFor(cents: number, ratePct: number): number {
  return Math.round((cents * ratePct) / 100);
}
