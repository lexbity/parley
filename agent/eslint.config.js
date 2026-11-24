export default [{
  files: ["**/*.mjs"],
  languageOptions: { ecmaVersion: "latest", sourceType: "module" },
  linterOptions: { reportUnusedDisableDirectives: "error" },
  rules: {
    "eqeqeq": ["error", "always"],
    "no-constant-condition": ["error", { checkLoops: "none" }],
    "no-dupe-keys": "error",
    "no-eval": "error",
    "no-unreachable": "error",
    "no-unused-vars": "error",
    "no-var": "error",
    "prefer-const": "error",
  },
}]
