"""Scripted review on the actual pinned Hermes, without a network model."""
import json
from pathlib import Path
from hermes_constants import get_hermes_home
from hermes_cli.plugins import _dispatch_pre_tool_call_hooks
from agent.background_review import load_background_review_settings
from agent import curator
from model_tools import get_tool_definitions
from tools.skill_provenance import set_current_write_origin, reset_current_write_origin
from tools.skill_manager_guards import _reset_background_review_read_marks
from tools.skills_tool import skill_view
from tools.skill_manager_tool import skill_manage

assert load_background_review_settings()[0]
assert curator.is_enabled() and not curator.get_consolidate()
names = {t['function']['name'] for t in get_tool_definitions(enabled_toolsets=['skills', 'memory'], quiet_mode=True)}
assert {'skill_manage', 'memory'} <= names
from agent.skill_utils import parse_frontmatter
managed = Path('/data/hermes/managed-skills')
for relative in ['finance/cocod-finite', 'finance/trading-agent-finite', 'nostr/nostr-agent-interface-cli-finite', 'music-generation-finite', 'research/polymarket-finite', 'social-media/x-api-finite']:
    fm, _ = parse_frontmatter((managed / relative / 'SKILL.md').read_text())
    assert fm['name'] == Path(relative).name and fm['description'], fm
    if 'metadata' in fm:
        assert isinstance(fm['metadata'], dict), fm
    if 'tags' in fm:
        assert isinstance(fm['tags'], list), fm
baseline = managed / 'research/model-council-finite'
local = get_hermes_home() / 'skills/research/model-council-finite'
before = {str(p.relative_to(baseline)): p.read_bytes() for p in baseline.rglob('*') if p.is_file()}
args = dict(action='patch', name='model-council-finite', old_string='# Model Council', new_string='# My Model Council')
token = set_current_write_origin('background_review')
_reset_background_review_read_marks()
try:
    blocked, _ = _dispatch_pre_tool_call_hooks('skill_manage', args)
    assert blocked and 'has not been loaded' in blocked, blocked
    assert not local.exists()
    assert json.loads(skill_view(args['name']))['success']
    blocked, _ = _dispatch_pre_tool_call_hooks('skill_manage', args)
    assert blocked is None, blocked
    result = json.loads(skill_manage(**args))
    assert result['success'], result
    assert '# My Model Council' in (local / 'SKILL.md').read_text()
    for path, content in before.items():
        assert (baseline / path).read_bytes() == content
        if path != 'SKILL.md':
            assert (local / path).read_bytes() == content
    assert json.loads(skill_view(args['name']))['skill_dir'] == str(local)
    args.update(old_string='# My Model Council', new_string='# My Revised Council')
    blocked, _ = _dispatch_pre_tool_call_hooks('skill_manage', args)
    assert blocked is None, blocked
    assert json.loads(skill_manage(**args))['success']
    assert '# My Revised Council' in (local / 'SKILL.md').read_text()
    # An unread supporting file still cannot be patched after the package fork.
    args.update(file_path='scripts/model_council.py', old_string='import', new_string='unused')
    result = json.loads(skill_manage(**args))
    assert result.get('_read_before_write_required'), result
finally:
    reset_current_write_origin(token)
print('Memory and skill review on; fresh-read guards preserved; complete writable local fork shadows immutable managed baseline.')
