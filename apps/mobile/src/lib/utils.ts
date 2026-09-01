// Real shadcn convention, not a new one -- apps/client/src/lib/utils.ts
// already has the identical cn(clsx(...), twMerge) pattern for the web
// app's own shadcn components; this is that same helper, ported to
// mobile now that class-variance-authority/clsx/tailwind-merge are real
// dependencies here too. Needed the moment any ui/ component accepts a
// className override prop (e.g. a caller passing extra spacing).
import { clsx, type ClassValue } from "clsx";
import { twMerge } from "tailwind-merge";

export function cn(...inputs: ClassValue[]) {
  return twMerge(clsx(inputs));
}
