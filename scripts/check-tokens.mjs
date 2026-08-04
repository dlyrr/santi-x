/**
 * Every `var(--token)` used anywhere in `src/` must resolve in every theme.
 *
 * A token that only one block defines does not fail the build, does not fail
 * `svelte-check`, and does not fail `vite build` — it renders as an unstyled
 * hole in three of the four palettes, and only in whichever window happens to
 * use it. This is the only check that catches that.
 *
 * Usage: node scripts/check-tokens.mjs   (exit 1 on any gap)
 */
import { readFileSync, readdirSync, statSync } from 'node:fs';
import { join, resolve, dirname } from 'node:path';
import { fileURLToPath } from 'node:url';

const root = resolve(dirname(fileURLToPath(import.meta.url)), '..');
const cssPath = join(root, 'src', 'app.css');
const css = readFileSync(cssPath, 'utf8');

/** The four palettes, named exactly as `app.css` selects them. */
const THEME_SELECTORS = [
  ':root[data-theme="dark"]',
  ':root[data-theme="claude"]',
  ':root[data-theme="claude-dark"]',
  ':root[data-theme="sharex"]'
];

/**
 * Body of the rule whose selector list contains `selector`, matched by walking
 * braces rather than by regex — a nested block would end the match early.
 */
function ruleBody(source, selector) {
  const at = source.indexOf(selector);
  if (at === -1) return null;
  const open = source.indexOf('{', at);
  if (open === -1) return null;
  let depth = 0;
  for (let i = open; i < source.length; i++) {
    if (source[i] === '{') depth++;
    else if (source[i] === '}' && --depth === 0) return source.slice(open + 1, i);
  }
  return null;
}

function definedIn(body) {
  const names = new Set();
  for (const m of body.matchAll(/(--[A-Za-z0-9_-]+)\s*:/g)) names.add(m[1]);
  return names;
}

const blocks = new Map();
for (const selector of THEME_SELECTORS) {
  const body = ruleBody(css, selector);
  if (body === null) {
    console.error(`FAIL: no theme block for ${selector} in src/app.css`);
    process.exit(1);
  }
  blocks.set(selector, definedIn(body));
}

/**
 * Every `var(--x)` in src/, and every `--x:` assignment outside the palettes,
 * both tracked per file. Locality is per file on purpose: a token a component
 * sets on its own element is that component's business, but the same name used
 * in a file that never sets it is relying on the palettes and must be in them.
 */
const used = new Map();
const assigned = new Map(); // token -> Set<file>
const EXT = /\.(svelte|css|ts|js|html)$/;

/** Palette bodies are the definitions under test, not evidence a token is local. */
const paletteBodies = THEME_SELECTORS.map((s) => ruleBody(css, s)).join('\n');

function walk(dir) {
  for (const entry of readdirSync(dir)) {
    const path = join(dir, entry);
    if (statSync(path).isDirectory()) walk(path);
    else if (EXT.test(entry)) scan(path);
  }
}

function scan(path) {
  const text = readFileSync(path, 'utf8');
  const rel = path.slice(root.length + 1).replaceAll('\\', '/');
  // Anything assigned anywhere else — a component rule, an inline
  // `style="--swatch: …"`, a string built in TS — is locally scoped and is not
  // the palettes' job to provide.
  const body = path === cssPath ? text.split(paletteBodies).join('\n') : text;
  const note = (token) => {
    if (!assigned.has(token)) assigned.set(token, new Set());
    assigned.get(token).add(rel);
  };
  for (const m of body.matchAll(/(--[A-Za-z0-9_-]+)\s*:/g)) note(m[1]);
  // `--m${token.slice(1)}` in SettingsView: the mock-preview names are built at
  // runtime from the palette list, so no literal assignment is ever written.
  if (/`--m\$\{/.test(body)) for (const t of PALETTE_TOKENS_IN(body)) note(`--m${t.slice(1)}`);

  text.split('\n').forEach((line, i) => {
    for (const m of line.matchAll(/var\(\s*(--[A-Za-z0-9_-]+)\s*(,?)/g)) {
      if (!used.has(m[1])) used.set(m[1], { sites: [], files: new Set(), bare: 0 });
      const entry = used.get(m[1]);
      entry.sites.push(`${rel}:${i + 1}`);
      entry.files.add(rel);
      if (!m[2]) entry.bare++; // no fallback: renders unstyled if nothing sets it
    }
  });
}

/** The `PALETTE_TOKENS` array literal a file declares, if it declares one. */
function PALETTE_TOKENS_IN(text) {
  const m = text.match(/PALETTE_TOKENS\s*(?::[^=]*)?=\s*\[([\s\S]*?)\]/);
  return m ? [...m[1].matchAll(/'(--[A-Za-z0-9_-]+)'|"(--[A-Za-z0-9_-]+)"/g)].map((x) => x[1] ?? x[2]) : [];
}

walk(join(root, 'src'));

const gaps = [];
const local = [];
for (const [token, { sites, files, bare }] of [...used].sort()) {
  const missing = THEME_SELECTORS.filter((s) => !blocks.get(s).has(token));
  if (!missing.length) continue;
  // Locally scoped: every file that reads it also sets it, so no reference is
  // left waiting on a palette.
  const owners = assigned.get(token) ?? new Set();
  const orphans = [...files].filter((f) => !owners.has(f));
  if (!orphans.length) {
    local.push(token);
    continue;
  }
  gaps.push({ token, missing, sites: sites.filter((s) => orphans.includes(s.split(':')[0])), bare });
}

const themed = used.size - local.length;
console.log(`app.css: 4 theme blocks, ${blocks.get(THEME_SELECTORS[0]).size} tokens each`);
console.log(`src/: ${used.size} distinct tokens used — ${themed} palette, ${local.length} locally scoped`);
if (local.length) console.log(`locally scoped (every file that reads them also sets them): ${local.join(' ')}`);

// The theme picker's mock previews read these back per theme via
// getComputedStyle, so one missing from a block silently renders that swatch
// with the live theme's colour instead of the one it is advertising.
const paletteList = PALETTE_TOKENS_IN(readFileSync(join(root, 'src/lib/views/SettingsView.svelte'), 'utf8'));
const previewGaps = paletteList.filter((t) => THEME_SELECTORS.some((s) => !blocks.get(s).has(t)));
console.log(`theme-picker PALETTE_TOKENS: ${paletteList.length} listed, ${previewGaps.length} missing from a block`);
if (previewGaps.length) {
  console.log(`FAIL: theme picker reads tokens no palette defines: ${previewGaps.join(' ')}`);
  process.exit(1);
}

if (!gaps.length) {
  console.log('\nOK: every palette --token used in src/ is defined in all four theme blocks.');
  process.exit(0);
}

console.log(`\nFAIL: ${gaps.length} token(s) missing from at least one theme:`);
for (const { token, missing, sites, bare } of gaps) {
  console.log(`  ${token}${bare ? ` (${bare} use(s) with no fallback)` : ''}`);
  console.log(`    missing from: ${missing.join(', ')}`);
  console.log(`    used at:      ${sites.slice(0, 4).join(', ')}${sites.length > 4 ? ` (+${sites.length - 4})` : ''}`);
}
process.exit(1);
