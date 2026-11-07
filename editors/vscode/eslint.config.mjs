import js from '@eslint/js';
import tseslint from 'typescript-eslint';

export default tseslint.config(
  { ignores: ['out/**', 'node_modules/**', '.vscode-test/**', 'test/**'] },
  js.configs.recommended,
  ...tseslint.configs.recommended,
);
