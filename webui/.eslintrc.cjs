/** @type {import("eslint").Linter.Config} */
module.exports = {
  root: true,
  env: { browser: true, es2022: true, node: true },
  parser: "@typescript-eslint/parser",
  parserOptions: { ecmaVersion: 2022, sourceType: "module", ecmaFeatures: { jsx: true } },
  plugins: ["@typescript-eslint", "react-hooks", "react-refresh"],
  extends: [
    "eslint:recommended",
    "plugin:@typescript-eslint/recommended",
    "plugin:react-hooks/recommended",
  ],
  rules: {
    "react-refresh/only-export-components": ["warn", { allowConstantExport: true }],
    "@typescript-eslint/no-unused-vars": ["warn", { argsIgnorePattern: "^_", varsIgnorePattern: "^_" }],
  },
  ignorePatterns: ["dist", "node_modules", ".eslintrc.cjs", "playwright-report"],
  overrides: [
    {
      // Playwright fixtures use a `use` callback that eslint mistakes for
      // a React Hook, and require a `{}` destructure for the empty
      // `inputs` slot. Both patterns are idiomatic Playwright.
      files: ["tests/e2e/**/*.ts"],
      rules: {
        "react-hooks/rules-of-hooks": "off",
        "no-empty-pattern": "off",
      },
    },
  ],
};
