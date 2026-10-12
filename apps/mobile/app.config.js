const { execFileSync } = require('node:child_process');
const path = require('node:path');
const root = path.resolve(__dirname, '../..');
const SHA = /^[0-9a-f]{40}$/;
function releaseIdentity(env = process.env, git = (...args) => execFileSync('git', ['-c', `safe.directory=${root}`, ...args], {
  cwd: root, encoding: 'utf8', timeout: 3000, maxBuffer: 128 * 1024,
}).trim()) {
  if (env.EAS_BUILD_GIT_COMMIT_HASH) {
    if (!SHA.test(env.EAS_BUILD_GIT_COMMIT_HASH)) throw new Error('Invalid EAS source identity');
    return { commit: env.EAS_BUILD_GIT_COMMIT_HASH, origin: 'EAS build metadata', tree: 'not independently checked' };
  }
  try {
    const commit = git('rev-parse', 'HEAD');
    const dirty = git('status', '--porcelain', '--untracked-files=all');
    return { commit: SHA.test(commit) && !dirty ? commit : null, origin: 'local Git', tree: dirty ? 'modified' : 'clean' };
  } catch { return { commit: null, origin: 'unavailable', tree: 'unknown' }; }
}
module.exports = ({ config }) => {
  const release = releaseIdentity();
  return { ...config, extra: { ...config.extra, stockspotterRelease: release },
    plugins: [...(config.plugins || []), ['./plugins/with-source-version', { commit: release.commit }]] };
};
module.exports.releaseIdentity = releaseIdentity;
