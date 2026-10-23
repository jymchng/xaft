// xaft browser page script.
//
// Evaluated by `engine.rs` inside a CDP *isolated world* created for each
// call, so the page's own JavaScript can neither see it nor tamper with the
// built-ins it uses (Array, querySelectorAll, getComputedStyle, ...). The DOM
// is shared with the page; the JavaScript globals are not.
//
// Entry point: `(op, arg) => result`. Expected conditions are reported as
// `{state: "..."}` values, never thrown, so the Rust side can retry or refuse
// with a fixed sentence. Nothing returned here is ever logged.
(function (op, arg) {
  "use strict";

  const norm = (s) => String(s == null ? "" : s).replace(/\s+/g, " ").trim();
  const clip = (s, n) => {
    const v = String(s == null ? "" : s);
    return v.length > n ? v.slice(0, n) : v;
  };

  // The document plus every open shadow root, document first.
  function roots() {
    const out = [document];
    for (let i = 0; i < out.length; i++) {
      const walker = document.createTreeWalker(out[i], NodeFilter.SHOW_ELEMENT);
      for (let node = walker.nextNode(); node; node = walker.nextNode()) {
        if (node.shadowRoot) out.push(node.shadowRoot);
      }
    }
    return out;
  }

  function cssAll(selector) {
    const out = [];
    for (const root of roots()) {
      for (const el of root.querySelectorAll(selector)) out.push(el);
    }
    return out;
  }

  function xpathAll(expr) {
    const result = document.evaluate(
      expr, document, null, XPathResult.ORDERED_NODE_SNAPSHOT_TYPE, null);
    const out = [];
    for (let i = 0; i < result.snapshotLength; i++) {
      const node = result.snapshotItem(i);
      if (node && node.nodeType === 1) out.push(node);
    }
    return out;
  }

  // `"exact"` / `'exact'` is a case-sensitive whole-text match; anything else
  // is a case-insensitive substring match (Playwright's `text=` semantics).
  function parseText(raw) {
    const t = raw.trim();
    const quoted = /^"([\s\S]*)"$/.exec(t) || /^'([\s\S]*)'$/.exec(t);
    if (quoted) return { needle: norm(quoted[1]), exact: true };
    return { needle: norm(t), exact: false };
  }

  const SKIP_TEXT = new Set(["SCRIPT", "STYLE", "NOSCRIPT", "TEMPLATE", "HEAD", "TITLE"]);

  function textOf(el) {
    return norm(el.textContent);
  }

  function containsText(el, needle, exact) {
    const t = textOf(el);
    return exact ? t.includes(needle) : t.toLowerCase().includes(needle.toLowerCase());
  }

  function equalsText(el, needle, exact) {
    const t = textOf(el);
    return exact ? t === needle : t.toLowerCase().includes(needle.toLowerCase());
  }

  // The innermost elements whose text matches. The walk only descends into
  // subtrees that still contain the needle, so it stays proportional to the
  // matching path rather than to the whole document.
  function byText(needle, exact) {
    if (!needle) return [];
    const results = [];
    const visit = (el) => {
      if (SKIP_TEXT.has(el.tagName) || !containsText(el, needle, exact)) return false;
      let childMatched = false;
      const children = el.shadowRoot ? [...el.shadowRoot.children, ...el.children] : el.children;
      for (const child of children) {
        if (visit(child)) childMatched = true;
      }
      if (!childMatched && equalsText(el, needle, exact)) {
        results.push(el);
        return true;
      }
      return childMatched;
    };
    if (document.body) visit(document.body);
    return results;
  }

  function unquote(raw) {
    const t = raw.trim();
    const m = /^"([\s\S]*)"$/.exec(t) || /^'([\s\S]*)'$/.exec(t);
    return m ? m[1] : t;
  }

  // Engines: `css=`, `xpath=` (or a leading `//`), `text=`, `id=`,
  // `data-testid=` / `data-test-id=` / `data-test=`, plain CSS (piercing open
  // shadow roots), and a trailing Playwright pseudo-class `:has-text("..")`,
  // `:text("..")` or `:text-is("..")`.
  function queryAll(sel) {
    let m;
    if ((m = /^css=([\s\S]*)$/.exec(sel))) return cssAll(m[1]);
    if ((m = /^xpath=([\s\S]*)$/.exec(sel))) return xpathAll(m[1]);
    if (/^\(*\/\//.test(sel) || sel.startsWith("..")) return xpathAll(sel);
    if ((m = /^text=([\s\S]*)$/.exec(sel))) {
      const t = parseText(m[1]);
      return byText(t.needle, t.exact);
    }
    if ((m = /^(id|data-testid|data-test-id|data-test)=([\s\S]*)$/.exec(sel))) {
      const v = unquote(m[2]);
      return cssAll(m[1] === "id" ? "#" + CSS.escape(v) : "[" + m[1] + '="' + CSS.escape(v) + '"]');
    }
    if ((m = /^([\s\S]*?):(has-text|text-is|text)\((["'])([\s\S]*)\3\)$/.exec(sel))) {
      const base = cssAll(m[1].trim() || "*");
      const needle = norm(m[4]);
      if (m[2] === "has-text") return base.filter((el) => containsText(el, needle, false));
      const exact = m[2] === "text-is";
      const hits = base.filter((el) => equalsText(el, needle, exact));
      return hits.filter((el) => !hits.some((other) => other !== el && el.contains(other)));
    }
    return cssAll(sel);
  }

  function resolve(sel) {
    try {
      const all = queryAll(sel);
      return { el: all.length ? all[0] : null };
    } catch (e) {
      return { invalid: true };
    }
  }

  function isVisible(el) {
    if (!el.isConnected) return false;
    const style = getComputedStyle(el);
    if (style.visibility === "hidden" || style.visibility === "collapse") return false;
    const r = el.getBoundingClientRect();
    return r.width > 0 && r.height > 0;
  }

  function isDisabled(el) {
    try {
      if (el.matches(":disabled")) return true;
    } catch (e) { /* not a form element */ }
    return !!el.closest('[aria-disabled="true"]');
  }

  const TYPE_INTO = new Set(["", "text", "search", "url", "tel", "email", "password", "number"]);
  const SET_VALUE = new Set(["color", "date", "time", "datetime-local", "month", "range", "week"]);
  const SENSITIVE = /(?:password|passwd|passcode|secret|token|api[_-]?key|authorization|cookie|credit[_-]?card|card[_-]?number|cvv|ssn)/i;
  const SENSITIVE_AUTOCOMPLETE = new Set([
    "current-password", "new-password", "one-time-code", "cc-number", "cc-csc",
    "cc-exp", "cc-exp-month", "cc-exp-year", "cc-name", "cc-given-name",
    "cc-family-name", "cc-type",
  ]);

  // How the element would be filled: "type" (focus + insert text),
  // "set" (assign value + input/change events), or null (not fillable).
  function fillMode(el) {
    const tag = el.tagName;
    if (tag === "INPUT") {
      const type = (el.getAttribute("type") || "").toLowerCase();
      if (TYPE_INTO.has(type)) return "type";
      if (SET_VALUE.has(type)) return "set";
      return null;
    }
    if (tag === "TEXTAREA") return "type";
    if (el.isContentEditable) return "type";
    return null;
  }

  // The selector is screened by the policy before this runs; this screens the
  // element it actually resolved to, so `#f1` cannot reach a password field.
  function isSensitiveElement(el) {
    if (el.tagName === "INPUT" && (el.getAttribute("type") || "").toLowerCase() === "password") {
      return true;
    }
    const autocomplete = (el.getAttribute("autocomplete") || "").toLowerCase();
    for (const token of autocomplete.split(/\s+/)) {
      if (SENSITIVE_AUTOCOMPLETE.has(token)) return true;
    }
    for (const attr of ["name", "id", "autocomplete", "aria-label", "placeholder"]) {
      const v = el.getAttribute(attr);
      if (v && SENSITIVE.test(v)) return true;
    }
    return false;
  }

  function isReadOnly(el) {
    if (el.isContentEditable) return false;
    return el.hasAttribute("readonly") || el.getAttribute("aria-readonly") === "true";
  }

  // The point at the centre of the element's visible part.
  function clickPoint(el) {
    const r = el.getBoundingClientRect();
    const left = Math.max(r.left, 0);
    const top = Math.max(r.top, 0);
    const right = Math.min(r.right, window.innerWidth);
    const bottom = Math.min(r.bottom, window.innerHeight);
    if (right <= left || bottom <= top) return null;
    return { x: (left + right) / 2, y: (top + bottom) / 2 };
  }

  function deepElementFromPoint(x, y) {
    let hit = document.elementFromPoint(x, y);
    while (hit && hit.shadowRoot) {
      const inner = hit.shadowRoot.elementFromPoint(x, y);
      if (!inner || inner === hit) break;
      hit = inner;
    }
    return hit;
  }

  function composedContains(el, node) {
    for (let n = node; n; n = n.parentNode || n.host) {
      if (n === el) return true;
    }
    return false;
  }

  const nextFrame = () => new Promise((resolveFrame) => {
    let done = false;
    const finish = () => { if (!done) { done = true; resolveFrame(); } };
    requestAnimationFrame(() => requestAnimationFrame(finish));
    setTimeout(finish, 50);
  });

  function sameRect(a, b) {
    return Math.abs(a.x - b.x) < 0.5 && Math.abs(a.y - b.y) < 0.5 &&
      Math.abs(a.width - b.width) < 0.5 && Math.abs(a.height - b.height) < 0.5;
  }

  // One actionability probe. `arg.action` is "click", "fill", "press",
  // "visible" or "text".
  async function probe(a) {
    let el;
    if (a.action === "text") {
      const hits = byText(norm(a.value), false);
      el = hits.find(isVisible) || null;
      return el ? { state: "ready" } : { state: "missing" };
    }
    const found = resolve(a.selector);
    if (found.invalid) return { state: "invalid_selector" };
    el = found.el;
    if (!el || !el.isConnected) return { state: "missing" };

    if (a.action === "press") {
      if (typeof el.focus === "function") el.focus({ preventScroll: true });
      return { state: "ready" };
    }
    if (a.action === "visible") {
      return isVisible(el) ? { state: "ready" } : { state: "hidden" };
    }

    if (typeof el.scrollIntoView === "function") {
      el.scrollIntoView({ block: "center", inline: "center", behavior: "instant" });
    }
    if (!isVisible(el)) return { state: "hidden" };
    if (isDisabled(el)) return { state: "disabled" };

    if (a.action === "fill") {
      const mode = fillMode(el);
      if (!mode) return { state: "not_fillable" };
      if (isSensitiveElement(el)) return { state: "sensitive" };
      if (isReadOnly(el)) return { state: "readonly" };
      if (mode === "set") {
        const setter = Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, "value").set;
        setter.call(el, a.value);
        el.dispatchEvent(new Event("input", { bubbles: true, composed: true }));
        el.dispatchEvent(new Event("change", { bubbles: true }));
        return { state: "filled" };
      }
      el.focus({ preventScroll: true });
      if (el.isContentEditable) {
        const range = document.createRange();
        range.selectNodeContents(el);
        const selection = window.getSelection();
        selection.removeAllRanges();
        selection.addRange(range);
      } else if (typeof el.select === "function") {
        el.select();
      }
      return { state: "ready" };
    }

    // click: the element must be stable and must receive the pointer event.
    const before = el.getBoundingClientRect();
    await nextFrame();
    if (!el.isConnected) return { state: "missing" };
    const after = el.getBoundingClientRect();
    if (!sameRect(before, after)) return { state: "unstable" };
    const point = clickPoint(el);
    if (!point) return { state: "hidden" };
    const hit = deepElementFromPoint(point.x, point.y);
    if (!hit || !composedContains(el, hit)) return { state: "obscured" };
    return { state: "ready", x: point.x, y: point.y };
  }

  // A CSS selector that addresses `el` from its root: `#id` when unique,
  // otherwise an `nth-of-type` path from the nearest uniquely-identified
  // ancestor. Empty when no short, unambiguous selector exists.
  function selectorFor(el) {
    const root = el.getRootNode();
    const unique = (sel) => {
      try { return cssAll(sel)[0] === el; } catch (e) { return false; }
    };
    if (el.id) {
      const byId = "#" + CSS.escape(el.id);
      if (root.querySelectorAll(byId).length === 1 && unique(byId)) return byId;
    }
    const parts = [];
    let node = el;
    while (node && node.nodeType === 1) {
      if (node !== el && node.id) {
        const anchor = "#" + CSS.escape(node.id);
        if (root.querySelectorAll(anchor).length === 1) {
          parts.unshift(anchor);
          break;
        }
      }
      const tag = node.localName;
      const parent = node.parentElement;
      if (!parent) {
        parts.unshift(tag);
        break;
      }
      const sameTag = Array.from(parent.children).filter((c) => c.localName === tag);
      parts.unshift(sameTag.length > 1 ? tag + ":nth-of-type(" + (sameTag.indexOf(node) + 1) + ")" : tag);
      node = parent;
    }
    const sel = parts.join(" > ");
    return sel.length <= 500 && unique(sel) ? sel : "";
  }

  function innerTextOf(el) {
    const t = el.innerText;
    return t == null ? norm(el.textContent) : t;
  }

  function snapshot(a) {
    const body = document.body || document.documentElement;
    const full = body ? String(body.innerText || "") : "";
    const cap = a.max_text_units;
    const links = [];
    for (const anchor of cssAll("a")) {
      if (links.length >= 50) break;
      links.push({
        href: anchor.getAttribute("href") || "",
        text: clip(innerTextOf(anchor), 500),
        selector: selectorFor(anchor),
      });
    }
    const controls = [];
    for (const el of cssAll("button,input,textarea,select")) {
      if (controls.length >= 50) break;
      if (el.tagName === "INPUT" && (el.getAttribute("type") || "").toLowerCase() === "hidden") continue;
      const item = {
        "aria-label": clip(el.getAttribute("aria-label") || "", 500),
        text: clip(innerTextOf(el), 500),
        selector: selectorFor(el),
        tag: el.localName,
      };
      for (const attr of ["type", "name", "placeholder", "role"]) {
        const v = el.getAttribute(attr);
        if (v) item[attr] = clip(v, 500);
      }
      if (isDisabled(el)) item.disabled = "true";
      controls.push(item);
    }
    return {
      url: location.href,
      title: document.title,
      text: full.length > cap ? full.slice(0, cap) : full,
      more: full.length > cap,
      links,
      controls,
    };
  }

  if (op === "state") return { url: location.href, title: document.title };
  if (op === "snapshot") return snapshot(arg);
  if (op === "probe") return probe(arg);
  return { state: "unknown_op" };
})
