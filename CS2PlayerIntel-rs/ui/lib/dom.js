// Small DOM helpers. Writes are skipped when nothing changed: an unchanged write still invalidates style
// and layout, which matters for a window drawn over the game.

export const byId = id => document.getElementById(id);

export function esc(value) {
  return String(value ?? '').replace(/[&<>'"]/g, c => ({ '&': '&amp;', '<': '&lt;', '>': '&gt;', "'": '&#39;', '"': '&quot;' }[c]));
}

export function setText(element, text) {
  if (element && element.textContent !== text) element.textContent = text;
}

export function setClass(element, name) {
  if (element && element.className !== name) element.className = name;
}

export function setHtml(element, html) {
  if (element && element.dataset.html !== html) {
    element.dataset.html = html;
    element.innerHTML = html;
  }
}

export function setHidden(element, hidden) {
  if (element && element.hidden !== hidden) element.hidden = hidden;
}
