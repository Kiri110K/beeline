import tseslint from "typescript-eslint";
import reactHooks from "eslint-plugin-react-hooks";

export default tseslint.config(
  { ignores: ["dist/", "src-tauri/", "eslint.config.js"] },
  ...tseslint.configs.strictTypeChecked,
  reactHooks.configs.flat["recommended-latest"],
  {
    languageOptions: {
      parserOptions: {
        projectService: true,
        tsconfigRootDir: import.meta.dirname,
      },
    },
    rules: {
      "@typescript-eslint/no-floating-promises": "error",
      "@typescript-eslint/switch-exhaustiveness-check": [
        "error",
        { considerDefaultExhaustiveForUnions: false },
      ],
      "@typescript-eslint/no-unnecessary-condition": "error",
      // The `as` ban is Beeline's `unsafe`: escape hatches are per-line
      // eslint-disable comments that state why the assertion is sound.
      "@typescript-eslint/consistent-type-assertions": [
        "error",
        { assertionStyle: "never" },
      ],
    },
  },
);
