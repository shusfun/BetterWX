import js from '@eslint/js';
import ts from 'typescript-eslint';
export default ts.config(js.configs.recommended, ...ts.configs.recommended, {
  files: ['ui/**/*.{ts,tsx}'],
  languageOptions: { globals: { window: 'readonly', navigator: 'readonly', console: 'readonly', setTimeout: 'readonly' } },
});
