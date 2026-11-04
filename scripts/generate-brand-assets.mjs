#!/usr/bin/env node
// Reproducible brand asset generation (`pnpm brand`).
//
// Input:  assets/branding/icon.svg (NEVER edited; its SHA-256 is checked below)
//         third_party/cache/fonts/SpaceGrotesk-Bold.ttf (run scripts/fetch-third-party.ps1)
// Output: assets/branding/generated/*  and  apps/desktop/src-tauri/icons/*
//
// - Wordmark "SyncWatch" is OUTLINED to SVG paths (no font needed to render the logo).
// - 16/24/32 px use a derived small variant (outer ring thickened + center dot), because the
//   inner rings of the master drop below 1 px at those sizes. 48 px and up use the master.
// - The .ico is assembled from the per-size PNGs.

import { createHash } from 'node:crypto';
import { mkdirSync, readFileSync, writeFileSync } from 'node:fs';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';
import { Resvg } from '@resvg/resvg-js';
import opentype from 'opentype.js';
import pngToIco from 'png-to-ico';

const root = join(dirname(fileURLToPath(import.meta.url)), '..');
const ICON = join(root, 'assets/branding/icon.svg');
const ICON_SHA256 = '1AFA3C57040CB841E4FCA8C25093857DED6E3E036E6E2E0153889C3D5E93310D';
const FONT = join(root, 'third_party/cache/fonts/SpaceGrotesk-Bold.ttf');
const OUT = join(root, 'assets/branding/generated');
const TAURI_ICONS = join(root, 'apps/desktop/src-tauri/icons');

const ACCENT = '#E8A33D';
const TEXT = '#F2F2F4';
const BG = '#0B0B0D';

function sha256(buf) {
  return createHash('sha256').update(buf).digest('hex').toUpperCase();
}

function fail(msg) {
  console.error(`brand: ${msg}`);
  process.exit(1);
}

const iconBuf = readFileSync(ICON);
if (sha256(iconBuf) !== ICON_SHA256) {
  fail('assets/branding/icon.svg changed. It must never be edited; restore it from git.');
}
const iconSvg = iconBuf.toString('utf8');

let fontBuf;
try {
  fontBuf = readFileSync(FONT);
} catch {
  fail('Space Grotesk Bold not found. Run: pwsh -NoProfile -File scripts/fetch-third-party.ps1');
}

mkdirSync(OUT, { recursive: true });
mkdirSync(TAURI_ICONS, { recursive: true });

// Small-size variant derived from the master geometry (viewBox 1080, center 540).
// Outer ring kept at the master's outer edge (r 450 + 30 = 480) but thickened; center dot
// enlarged; inner rings dropped.
const smallSvg = `<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 1080 1080">
  <circle cx="540" cy="540" r="410" fill="none" stroke="${ACCENT}" stroke-width="140"/>
  <circle cx="540" cy="540" r="190" fill="${ACCENT}"/>
</svg>
`;

function renderPng(svg, size) {
  const r = new Resvg(svg, {
    fitTo: { mode: 'width', value: size },
    background: 'rgba(0,0,0,0)',
    font: { loadSystemFonts: false },
  });
  return r.render().asPng();
}

// --- icons -----------------------------------------------------------------------------
const icoSizes = [16, 24, 32, 48, 64, 128, 256];
const pngs = {};
for (const size of [...icoSizes, 512, 1024]) {
  const svg = size <= 32 ? smallSvg : iconSvg;
  pngs[size] = renderPng(svg, size);
  writeFileSync(join(OUT, `icon-${size}.png`), pngs[size]);
}
writeFileSync(join(OUT, 'icon-small.svg'), smallSvg);
const ico = await pngToIco(icoSizes.map((s) => pngs[s]));
writeFileSync(join(OUT, 'icon.ico'), ico);

// Tauri expects these names.
writeFileSync(join(TAURI_ICONS, 'icon.ico'), ico);
writeFileSync(join(TAURI_ICONS, 'icon.png'), pngs[512]);
writeFileSync(join(TAURI_ICONS, '32x32.png'), pngs[32]);
writeFileSync(join(TAURI_ICONS, '128x128.png'), pngs[128]);
writeFileSync(join(TAURI_ICONS, '128x128@2x.png'), pngs[256]);

// --- horizontal lockup: icon left, outlined "SyncWatch" right ---------------------------
const ab = fontBuf.buffer.slice(fontBuf.byteOffset, fontBuf.byteOffset + fontBuf.byteLength);
const font = opentype.parse(ab);
const H = 1080; // icon box
const fontSize = 640;
const unitsPerEm = font.unitsPerEm;
const capHeight = (font.tables.os2?.sCapHeight || unitsPerEm * 0.7) * (fontSize / unitsPerEm);
const gap = 300;
const textX = H + gap;
const baseline = Math.round(H / 2 + capHeight / 2);
const textPath = font.getPath('SyncWatch', textX, baseline, fontSize);
const bbox = textPath.getBoundingBox();
const width = Math.ceil(bbox.x2 + 40);
// opentype.js 2.0.0's Path.toPathData() emits "NaN" for some glyph sequences even though the
// commands are finite, so the commands are serialized here instead.
const n = (v) => {
  if (!Number.isFinite(v)) fail('non-finite glyph coordinate');
  return (Math.round(v * 100) / 100).toString();
};
const d = textPath.commands
  .map((c) => {
    switch (c.type) {
      case 'M':
      case 'L':
        return `${c.type}${n(c.x)} ${n(c.y)}`;
      case 'Q':
        return `Q${n(c.x1)} ${n(c.y1)} ${n(c.x)} ${n(c.y)}`;
      case 'C':
        return `C${n(c.x1)} ${n(c.y1)} ${n(c.x2)} ${n(c.y2)} ${n(c.x)} ${n(c.y)}`;
      case 'Z':
        return 'Z';
      default:
        return fail(`unknown path command ${c.type}`);
    }
  })
  .join('');

// The master icon's inner markup, re-used verbatim inside a nested <svg>.
const iconInner = iconSvg
  .replace(/^[\s\S]*?<svg[^>]*>/, '')
  .replace(/<\/svg>\s*$/, '')
  .trim();

function lockup(bg) {
  const rect = bg ? `<rect width="100%" height="100%" fill="${bg}"/>` : '';
  return `<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 ${width} ${H}" width="${width}" height="${H}">
  ${rect}
  <svg x="0" y="0" width="${H}" height="${H}" viewBox="0 0 1080 1080">
    ${iconInner}
  </svg>
  <path fill="${TEXT}" d="${d}"/>
</svg>
`;
}

const lockupSvg = lockup(null);
writeFileSync(join(OUT, 'logo-lockup.svg'), lockupSvg);
writeFileSync(join(OUT, 'logo-lockup-on-dark.svg'), lockup(BG));
writeFileSync(join(OUT, 'logo-lockup-512w.png'), renderPng(lockupSvg, 512));
writeFileSync(join(OUT, 'logo-lockup-1024w.png'), renderPng(lockupSvg, 1024));
// Small UI copy of the lockup for the app header.
const uiAssets = join(root, 'apps/desktop/src/assets');
mkdirSync(uiAssets, { recursive: true });
writeFileSync(join(uiAssets, 'logo-lockup.svg'), lockupSvg);

console.log(`brand: wrote ${Object.keys(pngs).length} icon PNGs, icon.ico and the lockup (${width}x${H}).`);
