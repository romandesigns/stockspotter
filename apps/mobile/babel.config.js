// New as of the NativeWind redesign -- this project has never had a
// babel.config.js before (Expo SDK's own default preset applied
// implicitly with no config file at all). Adding this file opts out of
// that implicit default, so babel-preset-expo has to be listed
// explicitly here, not just nativewind/babel.
module.exports = function (api) {
  api.cache(true);
  return {
    presets: [
      ["babel-preset-expo", { jsxImportSource: "nativewind" }],
      "nativewind/babel",
    ],
  };
};
