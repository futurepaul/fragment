// An agent's mark: its picture when it has one (its agent fragment's
// avatar.png: docs/api.md, "An agent's name and picture"), else its image,
// tinted to its colour (shell.css, .agent-avatar). A picture that does not
// load leaves the tinted image.
export function avatar(identity, extra = '') {
  const mark = document.createElement('span');
  mark.className = `agent-avatar ${extra}`;
  mark.style.setProperty('--agent-color', identity.color);
  mark.setAttribute('aria-hidden', 'true');
  if (identity.picture) {
    const img = document.createElement('img');
    img.alt = '';
    img.src = identity.picture;
    img.onerror = () => {
      img.remove();
      mark.classList.remove('picture');
    };
    mark.classList.add('picture');
    mark.append(img);
  }
  return mark;
}
