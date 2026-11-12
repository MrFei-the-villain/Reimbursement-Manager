/* Reimbursement Manager GUI — vanilla JS, talks to the in-process Rust backend
   through Tauri commands (no HTTP sidecar). */
"use strict";

const tauri = window.__TAURI__;
const invoke = (cmd, args) => tauri.core.invoke(cmd, args);
let receipts = [];
let dirtyRows = new Set(); // Excel row numbers with unsaved edits
let statusTimer = null;

// ------------------------------------------------------------- helpers

function esc(text) {
  const div = document.createElement("div");
  div.textContent = text == null ? "" : String(text);
  return div.innerHTML;
}

// ------------------------------------------------------------- receipts

async function loadReceipts() {
  try {
    const body = await invoke("get_receipts");
    receipts = body.receipts || [];
    renderReceipts();
    renderStats();
    renderSummary();
  } catch (e) {
    console.error("get_receipts failed:", e);
  }
}

function renderReceipts() {
  const tbody = document.getElementById("receipts-body");
  dirtyRows.clear();
  updateSaveBar();
  if (!receipts.length) {
    tbody.innerHTML = '<tr><td colspan="5" class="empty">No receipts yet — drop an image above</td></tr>';
    return;
  }
  tbody.innerHTML = receipts
    .map(
      (r) => `
      <tr data-row="${r.row}" class="${r.needs_review ? "review" : ""}">
        <td><input class="cell" data-f="date" value="${esc(r.date) || ""}" placeholder="YYYY-MM-DD" /></td>
        <td><input class="cell cell-cur" data-f="currency" value="${esc(r.currency) || ""}" /></td>
        <td class="num"><input class="cell cell-amt" data-f="amount" type="number" step="0.01"
            value="${r.amount == null ? "" : r.amount}" /></td>
        <td><input class="cell" data-f="merchant" value="${esc(r.merchant) || ""}" /></td>
        <td class="row-actions"><button class="del-btn" data-del="${r.row}" title="Delete this receipt">&times;</button></td>
      </tr>`
    )
    .join("");
  tbody.querySelectorAll("tr[data-row]").forEach((tr) => {
    const row = Number(tr.dataset.row);
    tr.querySelectorAll("input.cell").forEach((inp) =>
      inp.addEventListener("input", () => {
        dirtyRows.add(row);
        updateSaveBar();
      }));
  });
  tbody.querySelectorAll("button.del-btn").forEach((btn) =>
    btn.addEventListener("click", () => deleteRow(Number(btn.dataset.del))));
}

async function deleteRow(row) {
  const target = receipts.find((r) => r.row === row);
  const label = target ? (target.merchant || target.date || `row ${row}`) : `row ${row}`;
  if (!confirm(`Delete receipt from "${label}"? This also removes it from receipts.xlsx.`)) return;
  try {
    await invoke("delete_receipt", { row });
    dirtyRows.delete(row);
    updateSaveBar();
  } catch (e) {
    const msg = document.getElementById("save-msg");
    msg.textContent = String(e) || "Delete failed";
    msg.style.color = "var(--warn-text)";
    setTimeout(() => (msg.textContent = ""), 4000);
  }
  await loadReceipts();
}

function updateSaveBar() {
  document.getElementById("save-changes").classList.toggle("hidden", dirtyRows.size === 0);
  document.getElementById("save-hint").textContent =
    dirtyRows.size ? `${dirtyRows.size} row(s) with unsaved edits` : "";
}

async function saveChanges() {
  if (!dirtyRows.size) return;
  const msg = document.getElementById("save-msg");
  const byRow = new Map(receipts.map((r) => [r.row, r]));
  const requests = [];
  for (const row of dirtyRows) {
    const orig = byRow.get(row) || {};
    const tr = document.querySelector(`tr[data-row="${row}"]`);
    if (!tr) continue;
    const get = (f) => tr.querySelector(`input[data-f="${f}"]`).value.trim();
    requests.push(
      invoke("edit_receipt", {
        row,
        patch: {
          date: get("date") || orig.date || "",
          currency: get("currency") || orig.currency || "",
          amount: get("amount"),
          merchant: get("merchant") || orig.merchant || "",
        },
      })
        .then(() => ({ ok: true }))
        .catch((e) => ({ ok: false, error: String(e) }))
    );
  }
  const results = await Promise.all(requests);
  const failed = results.find((r) => !r.ok);
  if (failed) {
    msg.textContent = failed.error || "Save failed";
    msg.style.color = "var(--warn-text)";
  } else {
    dirtyRows.clear();
    msg.textContent = "Saved ✓";
    msg.style.color = "var(--ok)";
  }
  setTimeout(() => (msg.textContent = ""), 3000);
  await loadReceipts();
}

// ------------------------------------------------------------- summary

function renderSummary() {
  const byCurrency = {};
  let review = 0;
  for (const r of receipts) {
    if (r.amount != null) {
      const cur = r.currency || "?";
      byCurrency[cur] = (byCurrency[cur] || 0) + Number(r.amount);
    }
    if (r.needs_review) review++;
  }
  document.getElementById("s-count").textContent = receipts.length;
  document.getElementById("s-total").textContent =
    Object.entries(byCurrency)
      .map(([cur, sum]) => `${cur} ${sum.toFixed(2)}`)
      .join("  ·  ") || "—";
  document.getElementById("s-review").textContent = review;
}

// ------------------------------------------------------------- stats

const _CURRENCY_PALETTE = [
  "#2563eb", "#f97316", "#16a34a", "#dc2626",
  "#9333ea", "#0891b2", "#ca8a04", "#db2777",
  "#475569", "#059669",
];

function _colorForCurrency(cur) {
  // stable hash so the same currency always gets the same color
  let h = 0;
  for (const c of String(cur || "?")) h = (h * 31 + c.charCodeAt(0)) | 0;
  return _CURRENCY_PALETTE[Math.abs(h) % _CURRENCY_PALETTE.length];
}

function renderStats() {
  // --- totals by currency (table, unchanged) ---
  const byCurrency = {};
  for (const r of receipts) {
    if (r.amount == null) continue;
    byCurrency[r.currency || "?"] = (byCurrency[r.currency || "?"] || 0) + Number(r.amount);
  }
  document.getElementById("currency-table").innerHTML =
    `<tr><th>Currency</th><th style="text-align:right">Total</th></tr>` +
    Object.entries(byCurrency)
      .map(([c, v]) => `<tr><td>${esc(c)}</td><td class="num">${v.toFixed(2)}</td></tr>`)
      .join("") || "";

  // --- top places (Place + Currency + Amount) ---
  const places = {};
  for (const r of receipts) {
    if (r.amount == null) continue;
    const key = `${r.merchant || "?"}|${r.currency || "?"}`;
    places[key] = places[key] || { place: r.merchant || "?", currency: r.currency || "?", amount: 0 };
    places[key].amount += Number(r.amount);
  }
  const placeList = Object.values(places).sort((a, b) => b.amount - a.amount).slice(0, 8);
  renderPlacesChart(placeList);

  // --- monthly spending by currency (Date + Currency + Amount) ---
  // group by (month, currency)
  const monthly = {};  // monthly[month][currency] = amount
  const currenciesSeen = new Set();
  for (const r of receipts) {
    if (r.amount == null || !r.date) continue;
    const month = String(r.date).slice(0, 7);
    const cur = r.currency || "?";
    monthly[month] = monthly[month] || {};
    monthly[month][cur] = (monthly[month][cur] || 0) + Number(r.amount);
    currenciesSeen.add(cur);
  }
  renderMonthlyByCurrency(monthly, currenciesSeen);
}

function renderPlacesChart(placeList) {
  const box = document.getElementById("places-chart");
  if (!placeList.length) {
    box.innerHTML = '<p class="hint">No receipts yet.</p>';
    return;
  }
  const W = 340, H = 220, padL = 100, padR = 10, padT = 14, padB = 14;
  const max = Math.max(...placeList.map((p) => p.amount));
  const rowH = (H - padT - padB) / placeList.length;
  const bw = W - padL - padR;
  const bars = placeList.map((p, i) => {
    const label = (p.place || "?").length > 14 ? (p.place || "?").slice(0, 13) + "…" : (p.place || "?");
    const hz = padT + i * rowH + rowH / 2 - 7;
    const w = Math.max(3, (p.amount / max) * bw);
    const color = _colorForCurrency(p.currency);
    return `<g>
      <text x="${padL - 6}" y="${hz + 11}" text-anchor="end" font-size="11" fill="#334155">${esc(label)}</text>
      <rect x="${padL}" y="${hz}" width="${w}" height="14" rx="3" fill="${color}" opacity="0.9">
        <title>${esc(p.place || "?")} · ${p.currency} ${p.amount.toFixed(2)}</title>
      </rect>
      <text x="${padL + w + 4}" y="${hz + 11}" font-size="10" fill="#64748b">${p.currency} ${p.amount.toFixed(0)}</text>
    </g>`;
  }).join("");
  box.innerHTML = `<svg viewBox="0 0 ${W} ${H}">${bars}</svg>`;
}

function renderMonthlyByCurrency(monthly, currenciesSeen) {
  const box = document.getElementById("monthly-chart");
  const months = Object.keys(monthly).sort();
  if (!months.length) {
    box.innerHTML = '<p class="hint">No dated receipts yet.</p>';
    return;
  }
  const currencies = Array.from(currenciesSeen).sort();
  // figure out group bar widths
  const W = 760, H = 220, padL = 36, padR = 14, padT = 26, padB = 34;
  const innerW = W - padL - padR;
  const innerH = H - padT - padB;
  const groupGap = 18;
  const groupW = (innerW - groupGap * (months.length - 1)) / months.length;
  const barW = Math.max(8, Math.min(44, groupW / Math.max(currencies.length, 1) - 4));

  // compute per-currency max so each scale is honest
  const maxByCur = {};
  for (const m of months) for (const c of currencies) {
    const v = monthly[m][c] || 0;
    if (v > (maxByCur[c] || 0)) maxByCur[c] = v;
  }
  // overall max across currencies (used for height scale — not additive, just sizing)
  const maxVal = Math.max(1, ...Object.values(maxByCur));

  let svg = "";
  months.forEach((m, gi) => {
    const groupX = padL + gi * (groupW + groupGap);
    let labelY = H - 10;
    currencies.forEach((cur, ci) => {
      const v = monthly[m][cur] || 0;
      const h = v > 0 ? Math.max(2, (v / maxVal) * innerH) : 0;
      const bx = groupX + ci * (barW + 2);
      const by = padT + innerH - h;
      const color = _colorForCurrency(cur);
      if (h > 0) {
        svg += `<rect x="${bx}" y="${by}" width="${barW}" height="${h}" rx="3" fill="${color}" opacity="0.9">
          <title>${m} · ${cur}: ${v.toFixed(2)}</title>
        </rect>`;
        if (h > 14) {
          svg += `<text x="${bx + barW / 2}" y="${by - 4}" text-anchor="middle" font-size="10" fill="#64748b">${v.toFixed(0)}</text>`;
        }
      }
    });
    // month label centered under group
    const center = groupX + (groupW - 2) / 2;
    svg += `<text x="${center}" y="${labelY}" text-anchor="middle" font-size="11" fill="#64748b">${m.slice(2)}</text>`;
  });

  // y-axis ticks
  const ticks = 4;
  for (let i = 0; i <= ticks; i++) {
    const y = padT + innerH - (innerH * i / ticks);
    const val = (maxVal * i / ticks).toFixed(0);
    svg += `<line x1="${padL}" y1="${y}" x2="${W - padR}" y2="${y}" stroke="#e2e8f0" stroke-width="1" stroke-dasharray="2,2"/>`;
    svg += `<text x="${padL - 4}" y="${y + 3}" text-anchor="end" font-size="10" fill="#94a3b8">${val}</text>`;
  }

  // legend
  const legendY = 6;
  let legendX = padL;
  const legendItems = currencies.map((cur) => {
    const color = _colorForCurrency(cur);
    const item = `<rect x="${legendX}" y="${legendY}" width="10" height="10" rx="2" fill="${color}"/><text x="${legendX + 14}" y="${legendY + 10}" font-size="11" fill="#475569">${esc(cur)}</text>`;
    legendX += 48;
    return item;
  }).join("");

  box.innerHTML = `<svg viewBox="0 0 ${W} ${H}">${legendItems}${svg}</svg>`;
}

// ------------------------------------------------------------- status

async function pollStatus() {
  try {
    const body = await invoke("get_status");
    setBadge("ok", body.engines_ready ? "running" : "starting…");
    document.getElementById("s-watcher").textContent =
      body.watcher_running ? "on" : "off";
    document.getElementById("activity").textContent =
      (body.recent || []).slice(-5).join("\n");
    // refresh table when the backend is busy or has pending work
    if (body.inbox > 0 || body.pending > 0) {
      clearTimeout(statusTimer);
      setTimeout(loadReceipts, 4000);
    }
  } catch {
    setBadge("err", "backend offline");
  }
}

function setBadge(kind, text) {
  const badge = document.getElementById("conn-badge");
  badge.textContent = text;
  badge.className = "badge " + kind;
}

// ------------------------------------------------------------- upload

async function addPath(path) {
  try {
    await invoke("add_path", { path });
  } catch (e) {
    console.error("add_path failed:", e);
  }
}

async function uploadFile(file) {
  const bytes = await file.arrayBuffer();
  try {
    await invoke("upload_image", { filename: file.name, data: new Uint8Array(bytes) });
  } catch (e) {
    console.error("upload failed:", e);
  }
}

function setupDropzone() {
  const zone = document.getElementById("dropzone");

  // Tauri drag-drop (file paths, works OS-wide in the webview)
  if (tauri?.webview) {
    tauri.webview.getCurrentWebview().onDragDropEvent(({ payload }) => {
      if (payload.type === "enter" || payload.type === "over") zone.classList.add("drag");
      else zone.classList.remove("drag");
      if (payload.type === "drop") {
        (payload.paths || []).forEach(addPath);
        setTimeout(loadReceipts, 2500);
      }
    });
  }

  // HTML5 fallback (file input)
  document.getElementById("file-input").addEventListener("change", async (e) => {
    for (const file of e.target.files) await uploadFile(file);
    e.target.value = "";
    setTimeout(loadReceipts, 2500);
  });
}

// ------------------------------------------------------------- settings

async function openPortal() {
  try {
    await invoke("open_portal");
  } catch (e) {
    const msg = document.getElementById("save-msg");
    msg.textContent = String(e) || "Could not open the site";
    msg.style.color = "var(--warn-text)";
    setTimeout(() => (msg.textContent = ""), 4000);
  }
}

async function loadSettings() {
  try {
    const body = await invoke("get_config");
    document.getElementById("set-currency").value = body.default_currency || "CNY";
    document.getElementById("set-portal").value = body.reimbursement_url || "";
    const c = body.cloud_ocr || {};
    document.getElementById("set-cloud-enabled").checked = !!c.enabled;
    document.getElementById("set-cloud-fallback").checked = c.fallback_on_low_confidence !== false;
    document.getElementById("set-base-url").value = c.base_url || "";
    document.getElementById("set-api-key").value = c.api_key || "";
    document.getElementById("set-model").value = c.model || "";
  } catch (e) {
    console.error("get_config failed:", e);
  }
}

async function saveSettings(e) {
  e.preventDefault();
  const payload = {
    default_currency: document.getElementById("set-currency").value,
    reimbursement_url: document.getElementById("set-portal").value.trim(),
    cloud_ocr: {
      enabled: document.getElementById("set-cloud-enabled").checked,
      fallback_on_low_confidence: document.getElementById("set-cloud-fallback").checked,
      base_url: document.getElementById("set-base-url").value.trim(),
      api_key: document.getElementById("set-api-key").value.trim(),
      model: document.getElementById("set-model").value.trim(),
    },
  };
  const msg = document.getElementById("settings-msg");
  try {
    await invoke("set_config", { patch: payload });
    msg.textContent = "Saved ✓";
    msg.style.color = "var(--ok)";
  } catch (err) {
    msg.textContent = String(err) || "Save failed";
    msg.style.color = "var(--warn-text)";
  }
  setTimeout(() => (msg.textContent = ""), 3000);
}

// ------------------------------------------------------------- tabs & init

function setupTabs() {
  document.querySelectorAll(".tab").forEach((btn) =>
    btn.addEventListener("click", () => {
      document.querySelectorAll(".tab").forEach((b) => b.classList.remove("active"));
      document.querySelectorAll(".tab-panel").forEach((p) => p.classList.remove("active"));
      btn.classList.add("active");
      document.getElementById("tab-" + btn.dataset.tab).classList.add("active");
      if (btn.dataset.tab === "settings") loadSettings();
    }));
}

async function init() {
  setupTabs();
  setupDropzone();
  document.getElementById("save-changes").addEventListener("click", saveChanges);
  document.getElementById("open-portal").addEventListener("click", openPortal);
  document.getElementById("settings-form").addEventListener("submit", saveSettings);

  await loadReceipts();
  await loadSettings();
  pollStatus();
  statusTimer = setInterval(pollStatus, 5000);
  // also poll receipts lightly so table stays fresh while files are processed
  setInterval(loadReceipts, 15000);
}

document.addEventListener("DOMContentLoaded", init);
