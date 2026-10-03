export const APP_EXAMPLES = [
  { id: 'notes', title: 'Notes', color: '#b18a30' },
  { id: 'tasks', title: 'Tasks', color: '#5d7bc3' },
  { id: 'calendar', title: 'Calendar', color: '#c9625e' },
];
const glyphs = {
  notes: '<path d="M7 7h10M7 12h10M7 17h7"/>',
  tasks: '<path d="m6 12 4 4 8-9"/>',
  calendar: '<rect x="5" y="6" width="14" height="14" rx="2"/><path d="M8 4v4M16 4v4M5 11h14"/>',
};
export function appIcon(id) {
  const app = APP_EXAMPLES.find(app => app.id === id);
  const icon = document.createElement('span');
  icon.className = 'app-icon';
  icon.setAttribute('aria-hidden', 'true');
  icon.style.setProperty('--app-color', app?.color ?? '#8f88bc');
  icon.innerHTML = `<svg viewBox="0 0 24 24" fill="none">${glyphs[id] ?? '<rect x="6" y="6" width="12" height="12" rx="2"/>'}</svg>`;
  return icon;
}
