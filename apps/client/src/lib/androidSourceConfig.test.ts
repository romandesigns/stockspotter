import { expect, test } from "bun:test";
const { releaseIdentity } = require("../../../mobile/app.config.js");
const { stampVersion } = require("../../../mobile/plugins/with-source-version.js");
const sha = "b".repeat(40);
test("source configuration refuses dirty or malformed commit stamps", () => {
  expect(releaseIdentity({}, (...args:string[]) => args[0] === "rev-parse" ? sha : "").commit).toBe(sha);
  expect(releaseIdentity({}, (...args:string[]) => args[0] === "rev-parse" ? sha : " M App.tsx").commit).toBe(null);
  expect(releaseIdentity({}, () => { throw new Error("no git"); }).commit).toBe(null);
  expect(releaseIdentity({ EAS_BUILD_GIT_COMMIT_HASH: sha }).origin).toBe("EAS build metadata");
  expect(releaseIdentity({ EAS_BUILD_GIT_COMMIT_HASH: sha }).tree).toBe("not independently checked");
  expect(() => releaseIdentity({ EAS_BUILD_GIT_COMMIT_HASH: "latest" })).toThrow();
});
test("native version stamp is idempotent and rejects ambiguous Gradle", () => {
  const original = 'android {\n defaultConfig {\n  versionName "1.0.0"\n }\n}\n';
  const stamped = stampVersion(original, sha);
  expect(stamped).toContain(`versionName "1.0.0+git.${sha}"`);
  expect(stampVersion(stamped,sha)).toBe(stamped);
  expect(stampVersion(original,null)).toBe(original);
  expect(() => stampVersion(original,"bad")).toThrow();
  expect(() => stampVersion("",sha)).toThrow();
  expect(() => stampVersion(original+original,sha)).toThrow();
  const cfg = require("../../../mobile/app.json").expo;
  expect(cfg.runtimeVersion).toBe("1.0.0");
});