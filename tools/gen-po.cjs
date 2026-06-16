// Fill in missing translations in every lang/<code>/LC_MESSAGES/vayou.po.
//
//   node tools/gen-po.cjs
//
// It extracts every @tr("…") string from ui/*.slint, and for each language .po
// translates the ones that aren't present yet (via the same Google endpoint the
// app uses for subtitles) and appends them. Existing entries are never touched,
// so it's safe to re-run after adding new UI strings. `{}` placeholders are
// preserved across translation.
"use strict";
const fs = require("fs");
const path = require("path");
const https = require("https");

const ROOT = path.resolve(__dirname, "..");
const UI = path.join(ROOT, "ui");
const LANGS = path.join(ROOT, "lang");
const PH = "XX1XX"; // sentinel that survives translation, restored to {} after

const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

function extractTr() {
  const set = new Set();
  for (const f of fs.readdirSync(UI)) {
    if (!f.endsWith(".slint")) continue;
    const txt = fs.readFileSync(path.join(UI, f), "utf8");
    const re = /@tr\("((?:[^"\\]|\\.)*)"/g;
    let m;
    while ((m = re.exec(txt))) set.add(m[1].replace(/\\"/g, '"').replace(/\\\\/g, "\\"));
  }
  return [...set];
}

function msgidsIn(po) {
  const ids = new Set();
  const re = /^msgid "((?:[^"\\]|\\.)*)"/gm;
  let m;
  while ((m = re.exec(po))) ids.add(m[1].replace(/\\"/g, '"').replace(/\\\\/g, "\\"));
  return ids;
}

function httpGet(url) {
  return new Promise((resolve, reject) => {
    https.get(url, { headers: { "User-Agent": "Mozilla/5.0" } }, (r) => {
      let d = "";
      r.on("data", (c) => (d += c));
      r.on("end", () => (r.statusCode >= 400 ? reject(new Error("HTTP " + r.statusCode)) : resolve(d)));
    }).on("error", reject);
  });
}

async function translate(text, tl) {
  const guarded = text.replace(/\{\}/g, PH);
  const url = `https://clients5.google.com/translate_a/t?client=dict-chrome-ex&sl=en&tl=${tl}&q=${encodeURIComponent(guarded)}`;
  for (let i = 0; i < 4; i++) {
    try {
      const body = JSON.parse(await httpGet(url));
      const out = typeof body[0] === "string" ? body[0] : body[0].join("");
      if (!out) throw new Error("empty");
      return out.replace(new RegExp(PH.split("").join("\\s*"), "g"), "{}");
    } catch (e) {
      if (i === 3) throw e;
      await sleep(900 * (i + 1));
    }
  }
}

const esc = (s) => s.replace(/\\/g, "\\\\").replace(/"/g, '\\"');

async function main() {
  const all = extractTr();
  for (const lang of fs.readdirSync(LANGS)) {
    const po = path.join(LANGS, lang, "LC_MESSAGES", "vayou.po");
    if (!fs.existsSync(po)) continue;
    const have = msgidsIn(fs.readFileSync(po, "utf8"));
    const missing = all.filter((s) => !have.has(s));
    if (!missing.length) { console.log(`${lang}: complete`); continue; }
    let block = "";
    for (const s of missing) {
      let t;
      try { t = await translate(s, lang); }
      catch (e) { console.log(`  ${lang} FAIL "${s}": ${e.message}`); t = s; }
      // Don't ship a translation that dropped the placeholder.
      if ((s.match(/\{\}/g) || []).length !== (t.match(/\{\}/g) || []).length) t = s;
      block += `\nmsgid "${esc(s)}"\nmsgstr "${esc(t)}"\n`;
      await sleep(150);
    }
    fs.appendFileSync(po, block, "utf8");
    console.log(`${lang}: +${missing.length}`);
  }
}
main();
