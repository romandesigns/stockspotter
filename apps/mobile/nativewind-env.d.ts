/// <reference types="nativewind/types" />

// nativewind/types only augments RN component props with `className` --
// it doesn't declare a module for importing a .css file itself (that's
// a Metro/webpack bundler convention, not something NativeWind's own
// type package assumes). Needed for index.ts's side-effect
// `import "./src/global.css"`.
declare module "*.css";
