// No emoji or pictographs in the app's own text (Milestone 8). Scans the UI sources and strings,
// index.html, the Tauri config (window title), every Rust source (the strings the app sends to
// the UI), the scripts (installer), and the built bundle (dist/) when it exists. Fails on
// Extended_Pictographic code points, variation selector 16, zero-width joiners, and the symbol
// blocks used as icons (arrows, technical, geometric shapes, dingbats, supplemental arrows,
// misc symbols and arrows). Exceptions: emoji-allowlist.txt ("path|U+XXXX|reason").
// Chat messages typed by people are user content and never stripped; they are not in these
// files.
//
//   node apps/desktop/scripts/emoji-scan.mjs              (everything, incl. dist/; exit 1 on a find)
//   node apps/desktop/scripts/emoji-scan.mjs --sources    (without the built bundle)
import { existsSync, readdirSync, readFileSync, statSync } from 'node:fs';
import { dirname, join, relative, sep } from 'node:path';
import { fileURLToPath } from 'node:url';

const APP = join(dirname(fileURLToPath(import.meta.url)), '..');
const REPO = join(APP, '..', '..');

export const BANNED =
  /\p{Extended_Pictographic}|️|‍|[←-⇿⋯⌀-⏿■-➿⤀-⥿⬀-⯿«»]/gu;

function walk(dir, exts, out) {
  if (!existsSync(dir)) return out;
  for (const n of readdirSync(dir)) {
    if (n === 'node_modules' || n === 'target') continue;
    const p = join(dir, n);
    if (statSync(p).isDirectory()) walk(p, exts, out);
    else if (exts.some((e) => n.endsWith(e))) out.push(p);
  }
  return out;
}

export function targets({ bundle = true } = {}) {
  const t = [];
  walk(join(APP, 'src'), ['.ts', '.tsx', '.css'], t);
  t.push(join(APP, 'index.html'), join(APP, 'src-tauri', 'tauri.conf.json'));
  walk(join(APP, 'src-tauri', 'src'), ['.rs'], t);
  walk(join(REPO, 'crates'), ['.rs'], t);
  walk(join(REPO, 'scripts'), ['.ps1', '.mjs', '.nsi', '.wxs'], t);
  if (bundle) walk(join(APP, 'dist'), ['.js', '.css', '.html'], t);
  return t;
}

export function allowlist() {
  const f = join(APP, 'emoji-allowlist.txt');
  if (!existsSync(f)) return [];
  return readFileSync(f, 'utf8')
    .split('\n')
    .map((l) => l.trim())
    .filter((l) => l && !l.startsWith('#'))
    .map((l) => {
      const [path, cp, ...why] = l.split('|');
      return { path: path.trim(), cp: cp.trim().toUpperCase(), why: why.join('|').trim() };
    });
}

/** Every banned code point found, minus the allowlist: "path:line U+XXXX c". */
export function scan(opts) {
  const allow = allowlist();
  const found = [];
  for (const f of targets(opts)) {
    const rel = relative(REPO, f).split(sep).join('/');
    const lines = readFileSync(f, 'utf8').split('\n');
    lines.forEach((line, i) => {
      for (const m of line.matchAll(BANNED)) {
        const cp = 'U+' + m[0].codePointAt(0).toString(16).toUpperCase().padStart(4, '0');
        if (allow.some((a) => a.cp === cp && rel.startsWith(a.path))) continue;
        found.push(`${rel}:${i + 1} ${cp} ${m[0]}`);
      }
    });
  }
  return found;
}

if (process.argv[1] && fileURLToPath(import.meta.url) === process.argv[1]) {
  const bundle = !process.argv.includes('--sources');
  const found = scan({ bundle });
  const n = targets({ bundle }).length;
  if (found.length) {
    console.error(`FAIL  emoji scan: ${found.length} pictograph(s) in the app's own text:`);
    for (const f of found) console.error('  ' + f);
    process.exit(1);
  }
  console.log(`PASS  emoji scan: ${n} files (sources, strings, Rust, scripts, bundle), none found`);
}
