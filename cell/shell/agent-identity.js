// An agent's mark: its image, tinted to its colour (shell.css, .agent-avatar).
export function avatar(identity, extra = '') {
  const mark = document.createElement('span');
  mark.className = `agent-avatar ${extra}`;
  mark.style.setProperty('--agent-color', identity.color);
  mark.setAttribute('aria-hidden', 'true');
  return mark;
}
