"""Image-owned for Hermes v0.21.7; never modifies upstream code.

Hermes chooses a session's terminal (local, or ssh to a user) at its first
command and keeps it, with the background processes it started, for the
session's life. A fence put up or let go while the gateway runs (decision 63,
proposed; docs/computers.md, "A fenced agent in our Hermes image") changes
what the profile's config names. Before each tool call that runs in the
terminal, a kept terminal that is not the one the profile now names is
ended, its background processes with it, so the call runs where the profile
says: a fenced agent's in its sandbox, never on in the hermes user's shell.

Fail closed: a call whose terminal cannot be checked is blocked.
"""

import logging

logger = logging.getLogger(__name__)

# the toolsets whose tools run in the session's terminal (Hermes runs file
# tools and execute_code through it)
TERMINAL_TOOLSETS = frozenset({'terminal', 'file', 'code_execution'})


def register(ctx):
    ctx.register_hook('pre_tool_call', follow_profile)


def follow_profile(tool_name, args, task_id='', **kwargs):
    try:
        from tools.registry import registry
        if registry.get_toolset_for_tool(tool_name) not in TERMINAL_TOOLSETS:
            return None
        end_stale(task_id or '')
    except Exception as e:
        return {'action': 'block', 'message': f'Its terminal could not be checked against its profile ({type(e).__name__}); try again.'}
    return None


def where(env):
    """Where a kept terminal runs: ('local',), ('ssh', user, host, port), or
    the name of another backend's class."""
    from tools.environments.local import LocalEnvironment
    from tools.environments.ssh import SSHEnvironment
    if isinstance(env, SSHEnvironment):
        return ('ssh', str(env.user), str(env.host), int(env.port))
    if isinstance(env, LocalEnvironment):
        return ('local',)
    return (type(env).__name__,)


def named(config):
    """Where the profile's config says its terminal runs, as `where` says it."""
    kind = config.get('env_type')
    if kind == 'ssh':
        return ('ssh', str(config.get('ssh_user', '')), str(config.get('ssh_host', '')), int(config.get('ssh_port', 22)))
    if kind == 'local':
        return ('local',)
    return None


def end_stale(task_id):
    from tools import terminal_tool as tt
    wanted = named(tt._get_env_config())
    if wanted is None:
        return  # a backend we never configure: not ours to judge
    keys = {tt._resolve_container_task_id(task_id or None)}
    if task_id:
        keys.add(task_id)
    with tt._env_lock:
        kept = {k: where(tt._active_environments[k]) for k in keys if k in tt._active_environments}
    stale = {k: w for k, w in kept.items() if w != wanted}
    if not stale:
        return
    from tools.process_registry import process_registry
    from tools.terminal_tool_lifecycle import cleanup_vm
    for key in keys:
        process_registry.kill_all(task_id=key, source='fragment-terminal-follows')
    for key, was in stale.items():
        logger.info('fragment-terminal-follows: ended %s\'s kept terminal %s; its profile names %s', key, was, wanted)
        cleanup_vm(key)
