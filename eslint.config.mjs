// ESLint for the TypeScript workspace (decl-ts, extension/vscode): the
// recommended rules of ESLint and typescript-eslint, with type information
// from each package's tsconfig.json, and Prettier's config last so no
// formatting rule fights the formatter. The site (Astro) and the grammar
// have their own tooling; the other directories are not TypeScript.
import js from '@eslint/js';
import tseslint from 'typescript-eslint';
import prettier from 'eslint-config-prettier';
import globals from 'globals';

export default tseslint.config(
  {
    ignores: [
      '**/node_modules/**',
      '**/dist/**',
      '**/.vscode-test/**',
      '**/.vscode-test-web/**',
      'site/**',
      'tree-sitter-decl/**',
      'decl-rs/**',
      'decl-py/**',
      'extension/zed/**',
      'extension/neovim/**',
      'extension/helix/**',
      'extension/emacs/**',
      'extension/vim/**',
      'extension/sublime/**',
      'extension/vscode/server/**',
      'spike/**',
      'extension/vscode/syntaxes/**',
      'tests/**',
      'docs/**',
      'examples/**',
      'packaging/**',
      'target/**',
    ],
  },
  js.configs.recommended,
  ...tseslint.configs.recommendedTypeChecked,
  {
    languageOptions: {
      parserOptions: { projectService: true, tsconfigRootDir: import.meta.dirname },
    },
    rules: {
      // The runtime's values, the AST it walks, and the JSON-RPC payloads of
      // the servers are dynamically typed today (`any`); typing them is a
      // separate change, so the rules that only flag `any` are off. Every
      // other recommended rule applies.
      '@typescript-eslint/no-explicit-any': 'off',
      '@typescript-eslint/no-unsafe-argument': 'off',
      '@typescript-eslint/no-unsafe-assignment': 'off',
      '@typescript-eslint/no-unsafe-call': 'off',
      '@typescript-eslint/no-unsafe-member-access': 'off',
      '@typescript-eslint/no-unsafe-return': 'off',
      // `X | any` documents the intended type where the value is dynamic (the same reason)
      '@typescript-eslint/no-redundant-type-constituents': 'off',
    },
  },
  {
    // the extension's test suites run under mocha's tdd interface
    files: ['extension/vscode/test/**/*.ts'],
    languageOptions: {
      globals: {
        suite: 'readonly',
        test: 'readonly',
        suiteSetup: 'readonly',
        suiteTeardown: 'readonly',
        setup: 'readonly',
        teardown: 'readonly',
      },
    },
  },
  {
    // the build and test scripts are plain ESM JavaScript for Node, outside the tsconfigs
    files: ['**/*.mjs'],
    ...tseslint.configs.disableTypeChecked,
    languageOptions: { globals: globals.node, parserOptions: { projectService: false } },
  },
  prettier,
);
