// Moved out of App.tsx (2026-09-04) so HaltMiniCard.tsx can use the same
// formatPrice App.tsx's own rows already use, without a second
// near-identical copy or a circular import back into App.tsx.
export const formatPrice = (value: number) => `$${value.toFixed(2)}`;
export const formatTime = (value: string) =>
  new Intl.DateTimeFormat("en-US", { timeZone: "America/New_York", hour: "numeric", minute: "2-digit" }).format(new Date(value));
