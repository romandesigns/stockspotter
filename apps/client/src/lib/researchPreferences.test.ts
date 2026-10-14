import { expect, test } from "bun:test";
import { readResearchPreference, saveResearchPreference, retryResearchRejection } from "./researchPreferences";

test("an explicit off choice overrides an enabled build and persists across reload", () => {
  let stored: string | null = null;
  const storage = { getItem: () => stored, setItem: (_key: string, value: string) => { stored = value; } };
  expect(readResearchPreference(storage, true)).toBe(true);
  expect(saveResearchPreference(storage, false)).toBe(true);
  expect(readResearchPreference(storage, true)).toBe(false);
  expect(saveResearchPreference(storage, true)).toBe(true);
  expect(readResearchPreference(storage, false)).toBe(true);
});

test("missing, malformed and inaccessible storage preserve the build default", () => {
  for (const fallback of [false, true]) {
    expect(readResearchPreference(null, fallback)).toBe(fallback);
    expect(readResearchPreference({ getItem: () => "bad" }, fallback)).toBe(fallback);
    expect(readResearchPreference({ getItem: () => { throw Error("denied"); } }, fallback)).toBe(fallback);
  }
  expect(saveResearchPreference(null, true)).toBe(false);
  expect(saveResearchPreference({ setItem: () => { throw Error("denied"); } }, true)).toBe(false);
});

test("only known capacity refusals retry; permanent or unknown refusals stop", () => {
  expect(retryResearchRejection("Experimental connection capacity reached")).toBe(true);
  expect(retryResearchRejection("Connection capacity reached")).toBe(true);
  for (const reason of ["Experimental strategies disabled", "Unauthorized", "Unsupported experimental client", "unsupported protocol version", "Too many authentication attempts", "new refusal", null, undefined]) {
    expect(retryResearchRejection(reason)).toBe(false);
  }
});
