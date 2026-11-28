// Phase & LUCID — theme switch and ⌘K documentation search. Zero dependencies, zero third-party network calls.
(() => {
  const root = document.documentElement;
  const saved = (() => { try { return localStorage.getItem("theme"); } catch { return null; } })();
  if (saved === "light" || saved === "dark") root.dataset.theme = saved;
  const themeButton = document.querySelector("[data-theme-toggle]");
  const label = () => {
    if (!themeButton) return;
    const t = root.dataset.theme || "system";
    themeButton.textContent = t === "dark" ? "☾" : t === "light" ? "☀︎" : "◐";
    themeButton.title = `Appearance: ${t}`;
  };
  label();
  themeButton?.addEventListener("click", () => {
    const next = { system: "light", light: "dark", dark: "system" }[root.dataset.theme || "system"];
    if (next === "system") delete root.dataset.theme; else root.dataset.theme = next;
    try { next === "system" ? localStorage.removeItem("theme") : localStorage.setItem("theme", next); } catch {}
    label();
  });

  const palette = document.querySelector(".palette");
  if (!palette) return;
  const input = palette.querySelector("input");
  const list = palette.querySelector("ul");
  const status = palette.querySelector(".status");
  const base = document.body.dataset.root || "";
  let index = null, results = [], active = 0;

  const load = async () => {
    if (index) return;
    try { index = await (await fetch(`${base}help/search-index.json`)).json(); } catch { index = []; }
  };
  const open = async () => { await load(); palette.classList.add("open"); input.value = ""; render(); input.focus(); };
  const close = () => palette.classList.remove("open");
  const esc = (s) => s.replace(/[&<>"]/g, (c) => ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;" }[c]));

  const search = (q) => {
    const terms = q.toLowerCase().split(/\s+/).filter(Boolean);
    if (!terms.length || q.trim().length < 2) return [];
    const scored = [];
    for (const r of index) {
      const title = `${r.title} ${r.heading || ""}`.toLowerCase();
      const hay = `${title} ${r.group} ${r.body}`.toLowerCase();
      if (!terms.every((t) => hay.includes(t))) continue;
      let score = 0;
      for (const t of terms) score += (title.includes(t) ? 5 : 0) + 1;
      if (!r.heading) score += 1;
      const at = r.body.toLowerCase().indexOf(terms[0]);
      const snippet = at >= 0 ? `…${r.body.slice(Math.max(0, at - 30), at + 95)}…` : r.body.slice(0, 115);
      scored.push({ ...r, score, snippet });
    }
    return scored.sort((a, b) => b.score - a.score).slice(0, 12);
  };

  const render = () => {
    const q = input.value.trim();
    results = index ? search(q) : [];
    active = 0;
    list.innerHTML = results.map((r, i) =>
      `<li><a href="${base}${r.url}" class="${i === 0 ? "active" : ""}"><span class="group">${esc(r.group)}</span>` +
      `${esc(r.heading || r.title)}<span class="snippet">${esc(r.snippet)}</span></a></li>`
    ).join("");
    status.textContent = !q ? "Search setup, APIs, CLI flags, and protocols · ↑↓ to move · ↵ to open"
      : results.length ? "" : `No results for “${q}”.`;
  };

  const move = (d) => {
    if (!results.length) return;
    const links = list.querySelectorAll("a");
    links[active]?.classList.remove("active");
    active = (active + d + results.length) % results.length;
    links[active]?.classList.add("active");
    links[active]?.scrollIntoView({ block: "nearest" });
  };

  input.addEventListener("input", render);
  input.addEventListener("keydown", (e) => {
    if (e.key === "ArrowDown") { e.preventDefault(); move(1); }
    else if (e.key === "ArrowUp") { e.preventDefault(); move(-1); }
    else if (e.key === "Enter") { const r = results[active]; if (r) location.href = `${base}${r.url}`; }
    else if (e.key === "Escape") close();
  });
  palette.querySelector(".scrim").addEventListener("click", close);
  document.querySelectorAll("[data-search]").forEach((b) => b.addEventListener("click", open));
  document.addEventListener("keydown", (e) => {
    const typing = /INPUT|TEXTAREA/.test(e.target.tagName) || e.target.isContentEditable;
    if ((e.metaKey || e.ctrlKey) && e.key.toLowerCase() === "k") { e.preventDefault(); open(); }
    else if (e.key === "/" && !typing && !palette.classList.contains("open")) { e.preventDefault(); open(); }
  });
})();
