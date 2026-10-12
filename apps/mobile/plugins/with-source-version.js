function stampVersion(contents, commit) {
  if (commit == null) return contents;
  if (!/^[0-9a-f]{40}$/.test(commit)) throw new Error('Invalid native source identity');
  const pattern = /(^\s*versionName\s+["'])([^"'\r\n]+)(["']\s*$)/gm;
  const matches = [...contents.matchAll(pattern)];
  if (matches.length !== 1) throw new Error('Expected exactly one generated Android versionName');
  return contents.replace(pattern, (_, before, version, after) =>
    before + version.replace(/\+git\.[0-9a-f]{40}$/, '') + '+git.' + commit + after);
}
module.exports = (config, { commit }) => require('expo/config-plugins').withAppBuildGradle(config, result => {
  if (result.modResults.language !== 'groovy') throw new Error('Unsupported Android Gradle dialect');
  result.modResults.contents = stampVersion(result.modResults.contents, commit);
  return result;
});
module.exports.stampVersion = stampVersion;
