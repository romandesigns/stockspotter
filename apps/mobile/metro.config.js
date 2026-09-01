// New as of the NativeWind redesign -- wraps Expo's own default Metro
// config with NativeWind's CSS-extraction transform. Like babel.config.js,
// this project relied on Expo's implicit default before; both are now
// explicit config files.
const { getDefaultConfig } = require("expo/metro-config");
const { withNativeWind } = require("nativewind/metro");

const config = getDefaultConfig(__dirname);

module.exports = withNativeWind(config, { input: "./src/global.css" });
