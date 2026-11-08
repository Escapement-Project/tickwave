// Copies the design system's tokens.css and fonts into dev/vendor/ (ignored by git) for the dev
// page. Run from web/: node scripts/vendor.ts [SITE_REPOSITORY]

import { cpSync, mkdirSync } from "node:fs";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const web = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const site = resolve(process.argv[2] ?? join(web, "../../escapement-project.github.io"));
const out = join(web, "dev/vendor");
mkdirSync(out, { recursive: true });
cpSync(join(site, "static/tokens.css"), join(out, "tokens.css"));
cpSync(join(site, "static/fonts"), join(out, "fonts"), { recursive: true });
console.log(`copied tokens.css and fonts/ from ${site} to ${out}`);
