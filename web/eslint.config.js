// 最小 flat config:JS/TS 推荐规则 + react-hooks 经典两条(rules-of-hooks / exhaustive-deps)。
// react-hooks v7 的推荐集包含 React Compiler 派生规则(如 set-state-in-effect),
// 现有代码按旧心智模型编写(如 GroupsCard 在 effect 里同步派生行),全面达标需大重构,
// 故只启用经典两条,其余待后续评估。
import js from "@eslint/js";
import globals from "globals";
import reactHooks from "eslint-plugin-react-hooks";
import tseslint from "typescript-eslint";

export default tseslint.config(
  { ignores: ["dist"] },
  js.configs.recommended,
  ...tseslint.configs.recommended,
  {
    files: ["src/**/*.{ts,tsx}"],
    languageOptions: { globals: globals.browser },
    plugins: { "react-hooks": reactHooks },
    rules: {
      "react-hooks/rules-of-hooks": "error",
      "react-hooks/exhaustive-deps": "warn",
    },
  },
);
