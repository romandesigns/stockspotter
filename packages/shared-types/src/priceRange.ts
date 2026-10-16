// The scanner's price range (2026-10-16, owner's request): $0.10-$20.50,
// both ends inclusive. Mirrors fast_funnel::SCAN_MIN_PRICE/SCAN_MAX_PRICE in
// Rust -- the funnel that fills the shortlist, the Bullish Momentum panel,
// the paper trader's momentum trigger and the base-rate engine all use it.
// The Ross Five Pillars badge keeps Ross's own $1-$20 rule on purpose.

export const SCAN_MIN_PRICE = 0.1;
export const SCAN_MAX_PRICE = 20.5;
export const SCAN_PRICE_LABEL = "$0.10–$20.50";

export function withinScanPrice(price: number | undefined | null): boolean {
  return typeof price === "number" && Number.isFinite(price) && price >= SCAN_MIN_PRICE && price <= SCAN_MAX_PRICE;
}
