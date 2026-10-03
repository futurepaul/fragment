// Shared visual identity for the local agent-per-chat prototype.
const colors = ['#a88bea', '#62c8af', '#eda978', '#80afe9', '#dc91b6', '#b7c878'];
export function defaultIdentity(name, index = 0) {
  const presets = {
    'welcome.skyler': { title: 'Website builder', preview: 'What are we making next?', color: colors[0] },
    'design-review.skyler': { title: 'Design partner', preview: 'A little more room to breathe.', color: colors[1] },
    'fresh-start.skyler': { title: 'Research partner', preview: 'Ready for a new rabbit hole.', color: colors[3] },
  };
  return { title: index ? `New agent ${index}` : 'New agent', preview: 'Give this agent its first task', color: colors[index % colors.length], ...presets[name] };
}
export function avatar(identity, extra = '') {
  const mark = document.createElement('span');
  mark.className = `agent-avatar ${extra}`;
  mark.style.setProperty('--agent-color', identity.color);
  mark.setAttribute('aria-hidden', 'true');
  return mark;
}
