# shop

A Next.js-shaped storefront used as an evaluation fixture for zcode. The
business logic lives in `lib/` and is tested with Node's built-in runner
(`node --test`, TypeScript via Node's type stripping). `lib/price.test.ts`
fails on purpose: the single-file-fix task asks the agent to fix it.

The eval setup script generates a large `node_modules/` and `.next/` so that
filtering is measured against a realistic tree.
