#!/usr/bin/env node
/**
 * xaft — docs-site build script (self-contained, zero-dependency Node).
 *
 * What it does:
 *   1. Scans docs/ for .md files (index, guides/*, reference/*, contributing).
 *   2. Renders each markdown file to a standalone HTML page (minimal parser:
 *      headings, paragraphs, lists, tables, code blocks, links, blockquotes).
 *   3. Validates every internal link (relative paths and anchors) — fails with
 *      a non-zero exit when a link target is missing, so CI can gate on it.
 *   4. Generates llms.txt (index) and llms-full.txt (concatenated docs) at the
 *      repo root, mirroring agenthicc's layout.
 *
 * Usage:
 *   node scripts/docs-site.cjs            # build site/ + llms*.txt
 *   node scripts/docs-site.cjs --check    # only verify links + llms freshness
 *
 * Outputs:
 *   site/               static HTML (open site/index.html)
 *   llms.txt            LLM-facing doc index
 *   llms-full.txt       LLM-facing full doc text
 */
"use strict";

const fs = require("fs");
const path = require("path");

const ROOT = path.resolve(__dirname, "..");
const DOCS = path.join(ROOT, "docs");
const SITE = path.join(ROOT, "site");
const CHECK_ONLY = process.argv.includes("--check");

const TITLE = "xaft — Rust-native coding agent runtime";

// ── Minimal markdown → HTML ────────────────────────────────────────────────

function escapeHtml(s) {
  return s
    .replace(/&/g, "&amp;")
    .replace(/</g, "&lt;")
    .replace(/>/g, "&gt;")
    .replace(/"/g, "&quot;");
}

function inline(text) {
  // `code` spans
  let out = text.replace(/`([^`]+)`/g, (_, c) => `<code>${escapeHtml(c)}</code>`);
  // **bold**
  out = out.replace(/\*\*([^*]+)\*\*/g, "<strong>$1</strong>");
  // [text](target)
  out = out.replace(/\[([^\]]+)\]\(([^)]+)\)/g, (_, t, target) => {
    return `<a href="${escapeHtml(target)}">${t}</a>`;
  });
  return out;
}

/** Parse a markdown doc into HTML. Returns { html, title, links } where links
 *  are internal relative targets (path + optional #anchor). */
function renderMarkdown(src, filePath) {
  const lines = src.split("\n");
  const links = [];
  const out = [];
  let inCode = false;
  let codeLang = "";
  let codeBuf = [];
  let inTable = false;
  let tableBuf = [];

  const flushTable = () => {
    if (tableBuf.length === 0) return;
    const rows = tableBuf.map((r) =>
      r
        .map((c) => `<td>${inline(c.trim())}</td>`)
        .join("")
    );
    out.push(`<table><tbody>${rows.map((r) => `<tr>${r}</tr>`).join("")}</tbody></table>`);
    tableBuf = [];
    inTable = false;
  };

  for (const raw of lines) {
    const line = raw;
    if (line.startsWith("```")) {
      if (!inCode) {
        inCode = true;
        codeLang = line.slice(3).trim();
        codeBuf = [];
      } else {
        inCode = false;
        out.push(
          `<pre><code class="language-${escapeHtml(codeLang || "text")}">${escapeHtml(
            codeBuf.join("\n")
          )}</code></pre>`
        );
      }
      continue;
    }
    if (inCode) {
      codeBuf.push(line);
      continue;
    }

    // Tables: a header row followed by a |---| separator.
    if (line.trim().startsWith("|") && line.trim().endsWith("|")) {
      const cells = line.trim().slice(1, -1).split("|");
      if (cells.every((c) => /^[\s:-]+$/.test(c))) {
        // separator row — drop it
        continue;
      }
      if (!inTable) {
        inTable = true;
        tableBuf = [];
      }
      tableBuf.push(cells);
      continue;
    } else {
      flushTable();
    }

    const trimmed = line.trim();
    if (trimmed.startsWith("### ")) {
      out.push(`<h3>${inline(trimmed.slice(4))}</h3>`);
    } else if (trimmed.startsWith("## ")) {
      out.push(`<h2>${inline(trimmed.slice(3))}</h2>`);
    } else if (trimmed.startsWith("# ")) {
      out.push(`<h1>${inline(trimmed.slice(2))}</h1>`);
    } else if (trimmed.startsWith("> ")) {
      out.push(`<blockquote>${inline(trimmed.slice(2))}</blockquote>`);
    } else if (trimmed.startsWith("- ") || trimmed.startsWith("* ")) {
      out.push(`<li>${inline(trimmed.slice(2))}</li>`);
    } else if (/^\d+\.\s/.test(trimmed)) {
      out.push(`<li>${inline(trimmed.replace(/^\d+\.\s/, ""))}</li>`);
    } else if (trimmed === "---") {
      out.push("<hr/>");
    } else if (trimmed === "") {
      out.push("</ul>".repeat(0));
      out.push("\n");
    } else {
      // capture internal links from inline text
      const re = /\[[^\]]+\]\(([^)]+)\)/g;
      let m;
      while ((m = re.exec(line))) {
        const target = m[1];
        if (!/^(https?:|mailto:|#)/.test(target)) {
          links.push({ target, file: filePath });
        }
      }
      out.push(`<p>${inline(trimmed)}</p>`);
    }
  }
  flushTable();
  return { html: out.join("\n"), links };
}

function slugify(h) {
  return h
    .toLowerCase()
    .replace(/[^a-z0-9\s-]/g, "")
    .trim()
    .replace(/\s+/g, "-");
}

function pageHtml(title, bodyHtml) {
  return `<!DOCTYPE html>
<html lang="en">
<head>
<meta charset="utf-8"/>
<meta name="viewport" content="width=device-width, initial-scale=1"/>
<title>${escapeHtml(title)} — ${escapeHtml(TITLE)}</title>
<style>
  body { font-family: Inter, system-ui, sans-serif; max-width: 820px; margin: 2rem auto; padding: 0 1rem; line-height: 1.6; color: #1a1a1a; }
  pre { background: #f4f4f4; padding: 0.8rem; border-radius: 6px; overflow-x: auto; }
  code { background: #f4f4f4; padding: 0.1rem 0.3rem; border-radius: 4px; }
  table { border-collapse: collapse; width: 100%; margin: 1rem 0; }
  th, td { border: 1px solid #ddd; padding: 0.4rem 0.6rem; text-align: left; }
  blockquote { border-left: 3px solid #f97316; margin-left: 0; padding-left: 1rem; color: #555; }
  a { color: #f97316; }
</style>
</head>
<body>
${bodyHtml}
<hr/>
<p><em>Generated by <code>scripts/docs-site.cjs</code> — source in <code>docs/</code>.</em></p>
</body>
</html>
`;
}

// ── Collect docs ───────────────────────────────────────────────────────────

function collectMd(dir, base) {
  const out = [];
  for (const entry of fs.readdirSync(dir, { withFileTypes: true })) {
    const full = path.join(dir, entry.name);
    if (entry.isDirectory()) {
      out.push(...collectMd(full, base));
    } else if (entry.name.endsWith(".md")) {
      const rel = path.relative(base, full).split(path.sep).join("/");
      out.push({ file: rel, abs: full });
    }
  }
  return out;
}

// ── Link validation ────────────────────────────────────────────────────────

function validateLinks(rendered) {
  const byFile = new Map(rendered.map((r) => [r.file, r]));
  const errors = [];
  for (const r of rendered) {
    for (const { target, file } of r.links) {
      const [pathPart, anchor] = target.split("#");
      const decoded = pathPart ? decodeURIComponent(pathPart) : "";
      if (!decoded || decoded.endsWith(".md")) {
        // Resolve the target two ways and accept either:
        // 1. file-relative (target next to the current file)
        // 2. docs-root-relative (target as written from docs/)
        const baseDir = path.posix.dirname(file);
        const candidates = [
          path.posix.normalize(path.posix.join(baseDir, decoded || "index.md")),
          path.posix.normalize(decoded || "index.md"),
        ];
        const resolved = candidates.find(
          (c) => byFile.get(c) || c === "index.md" || byFile.get(path.posix.join(c, "index.md"))
        );
        if (!resolved) {
          errors.push(`  ${file}: broken link → ${target}`);
          continue;
        }
        const targetEntry = byFile.get(resolved) || byFile.get(path.posix.join(resolved, "index.md"));
        if (targetEntry && anchor) {
          const src = fs.readFileSync(targetEntry.abs, "utf8");
          const headers = [...src.matchAll(/^#{1,6}\s+(.+)$/gm)].map((m) =>
            slugify(m[1].replace(/[`*]/g, ""))
          );
          if (!headers.includes(anchor)) {
            errors.push(`  ${file}: broken anchor #${anchor} in ${resolved}`);
          }
        }
      }
    }
  }
  return errors;
}

// ── llms.txt generation ────────────────────────────────────────────────────

function buildLlms(rendered) {
  const guides = rendered
    .filter((r) => r.file.startsWith("guides/"))
    .sort((a, b) => a.file.localeCompare(b.file));
  const refs = rendered
    .filter((r) => r.file.startsWith("reference/"))
    .sort((a, b) => a.file.localeCompare(b.file));
  let out = `# xaft\n\n> ${TITLE}: plans, executes, verifies, and delivers code changes with\n> transactional safety, real-time observability, and multi-agent orchestration.\n\n## Docs index\n\n- [Home](docs/index.md)\n`;
  for (const g of guides) out += `- [${g.file.replace("guides/", "")}](docs/${g.file})\n`;
  for (const r of refs) out += `- [${r.file.replace("reference/", "")}](docs/${r.file})\n`;
  out += `- [Contributing](docs/contributing.md)\n\n## Important symbols\n\n- TUI: \`crates/xaft-tui\` — triggers (\`/\`, \`@\`, \`$\`, \`#\`), paste placeholder,\n  tool-group collapse, resume-tail replay, mode cycle Safe → Plan → Yolo,\n  telemetry, approvals.\n- Runtime: \`crates/xaft-runtime\` — event loop, providers, orchestration,\n  session store, compactor.\n- Tools: \`crates/xaft-tools\` — fs/git/shell tools + dynamic factory.\n- Config: \`crates/xaft-config\` — TOML load/merge, env overrides, hot reload.\n- Modes: Safe (read-only sandbox), Plan (read-only plan), Yolo/Auto (full).\n  Aliases: yolo→auto, ask/guard→safe, review→plan; debug rejected.\n`;
  return out;
}

function buildLlmsFull(rendered) {
  let out = `# xaft\n\n> ${TITLE} — full documentation.\n\n`;
  const ordered = rendered.sort((a, b) => a.file.localeCompare(b.file));
  for (const r of ordered) {
    const src = fs.readFileSync(r.abs, "utf8");
    out += `\n\n---\n\n# ${r.file}\n\n${src.trim()}\n`;
  }
  return out;
}

// ── Main ───────────────────────────────────────────────────────────────────

function main() {
  const files = collectMd(DOCS, DOCS);
  if (files.length === 0) {
    console.error("docs-site: no markdown files found under docs/");
    process.exit(1);
  }

  const rendered = files.map((f) => {
    const src = fs.readFileSync(f.abs, "utf8");
    const { html, links } = renderMarkdown(src, f.file);
    const title = src.match(/^#\s+(.+)$/m)?.[1] || f.file;
    return { ...f, html, links, title };
  });

  // 1. Validate internal links.
  const errors = validateLinks(rendered);
  if (errors.length > 0) {
    console.error("docs-site: broken internal links:\n" + errors.join("\n"));
    process.exit(1);
  }
  console.log(`docs-site: ${rendered.length} docs, 0 broken internal links`);

  // 2. Generate llms.txt + llms-full.txt.
  const llms = buildLlms(rendered);
  const llmsFull = buildLlmsFull(rendered);
  fs.writeFileSync(path.join(ROOT, "llms.txt"), llms);
  fs.writeFileSync(path.join(ROOT, "llms-full.txt"), llmsFull);
  console.log("docs-site: wrote llms.txt + llms-full.txt");

  if (CHECK_ONLY) {
    console.log("docs-site: check passed");
    return;
  }

  // 3. Render the site.
  fs.rmSync(SITE, { recursive: true, force: true });
  for (const r of rendered) {
    const outPath = path.join(SITE, r.file.replace(/\.md$/, ".html"));
    fs.mkdirSync(path.dirname(outPath), { recursive: true });
    fs.writeFileSync(outPath, pageHtml(r.title, r.html));
  }
  // Copy assets (logos/screenshots).
  const assetsSrc = path.join(ROOT, "assets");
  if (fs.existsSync(assetsSrc)) {
    fs.cpSync(assetsSrc, path.join(SITE, "assets"), { recursive: true });
  }
  // Home convenience.
  fs.copyFileSync(path.join(SITE, "index.html"), path.join(SITE, "index.html"));
  console.log(`docs-site: built ${rendered.length} pages into site/`);
}

main();
