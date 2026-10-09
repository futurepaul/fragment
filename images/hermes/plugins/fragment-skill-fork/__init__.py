"""Image-owned copy-on-write for Hermes v0.21.6; never modifies upstream code.

Only our two immutable external roots are forked. Hermes' read-before-write,
validation, security scan and deletion rules still own the actual mutation.
"""
import fcntl
import os
from pathlib import Path
import shutil
import stat
import tempfile

ROOTS = (Path('/data/hermes/managed-skills'), Path('/var/lib/fragment-run/platform-skills'))


def register(ctx):
    ctx.register_hook('pre_tool_call', fork_before_write)


def fork_before_write(tool_name, args, **kwargs):
    if tool_name != 'skill_manage':
        return
    if args.get('operations') is not None:
        from tools.skill_manager_tool import _find_skill
        for op in args['operations'] if isinstance(args['operations'], list) else []:
            if not isinstance(op, dict):
                continue
            found = _find_skill(op.get('name') or args.get('name', ''))
            if found and any(found['path'].resolve().is_relative_to(r) for r in ROOTS):
                return {'action': 'block', 'message': 'Fork external skills with a single flat skill_manage write first; then batch operations on the local copy.'}
        return
    if args.get('action') not in {'patch', 'edit', 'write_file', 'remove_file'}:
        return
    from hermes_constants import get_hermes_home
    from tools.skill_manager_tool import _find_skill, _resolve_supporting_file
    from tools.skill_manager_guards import (
        _background_review_read_before_write_guard, _background_review_has_read,
        mark_background_review_skill_read,
    )

    existing = _find_skill(args.get('name', ''))
    if not existing:
        return  # Hermes supplies the normal not-found error.
    source = existing['path'].resolve()
    root = next((r for r in ROOTS if source.is_relative_to(r)), None)
    if root is None:
        return  # User-local skills and other external sources keep Hermes' behavior.
    action = args['action']
    label = args.get('file_path') or 'SKILL.md'
    target, error = _resolve_supporting_file(source, label)
    if error:
        return {'action': 'block', 'message': error['error']}
    if target.exists():
        guard = _background_review_read_before_write_guard(args['name'], target, action, label)
        if guard:
            return {'action': 'block', 'message': guard['error']}
    local = get_hermes_home() / 'skills'
    destination = local / source.relative_to(root)
    if local.is_symlink() or not destination.resolve().is_relative_to(local.resolve()):
        return {'action': 'block', 'message': 'The local skills path redirects outside the profile skills tree.'}
    scratch = None
    try:
        local.mkdir(parents=True, exist_ok=True)
        # Separate processes/review threads must never overwrite an existing local fork.
        cache = get_hermes_home() / 'cache/skill-forks'
        cache.mkdir(parents=True, exist_ok=True)
        with (cache / 'fork.lock').open('a') as lock:
            fcntl.flock(lock, fcntl.LOCK_EX)
            if destination.exists() or destination.is_symlink():
                return {'action': 'block', 'message': 'A local skill appeared during the fork. Read it with skill_view and retry.'}
            destination.parent.mkdir(parents=True, exist_ok=True)
            scratch = Path(tempfile.mkdtemp(prefix='fork-', dir=cache))
            count, size = 0, 0
            for entry in source.rglob('*'):
                mode = entry.lstat().st_mode
                if stat.S_ISLNK(mode) or not (stat.S_ISREG(mode) or stat.S_ISDIR(mode)):
                    raise ValueError('Skill packages must contain only regular files and directories')
                copy = scratch / entry.relative_to(source)
                if entry.is_dir():
                    copy.mkdir(parents=True, exist_ok=True)
                else:
                    count += 1
                    size += entry.stat().st_size
                    if count > 1000 or size > 20 * 1024 * 1024:
                        raise ValueError('Skill package exceeds fork bounds (1000 files, 20 MiB)')
                    copy.parent.mkdir(parents=True, exist_ok=True)
                    shutil.copyfile(entry, copy)
                    copy.chmod(0o755 if mode & 0o111 else 0o644)
            # A managed sync may replace the baseline during the copy. Never
            # publish a partial or mixed package from that race.
            originals = {p.relative_to(source): p.read_bytes() for p in source.rglob('*') if p.is_file()}
            copies = {p.relative_to(scratch): p.read_bytes() for p in scratch.rglob('*') if p.is_file()}
            if originals != copies or 'SKILL.md' not in {str(p) for p in copies}:
                raise ValueError('Managed skill changed during the fork; reread and retry')
            os.rename(scratch, destination)
            scratch = None
            # Transfer only exact-file fresh reads, never inferred transcript content.
            for entry in source.rglob('*'):
                if entry.is_file() and _background_review_has_read(entry):
                    mark_background_review_skill_read(destination / entry.relative_to(source))
    except (OSError, ValueError) as exc:
        return {'action': 'block', 'message': f'Cannot fork this managed skill into the profile: {exc}'}
    finally:
        if scratch is not None:
            shutil.rmtree(scratch)
