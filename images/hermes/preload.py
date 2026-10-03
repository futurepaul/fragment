# The Hermes image's gateway preload (spike S3b, cut e): Hermes' heavy imports
# start while the platform restores /data and s6 runs its setup; once
# hermes-boot says go, the gateway runs in this same process, as the main
# program (no s6-supervised gateway, so no second one from a restored
# gateway_state.json). Started by `hermes-boot pre-init` as the hermes user.
#
# This file is the image's, not the repo's tooling: it runs inside Hermes'
# own Python, which only it can preload.
import hashlib
import os
import sys
import time

RUN = os.environ.get("FRAGMENT_RUN", "/var/lib/fragment-run")
GO = os.path.join(RUN, "go")

# The heavy part: the gateway and its Relay platform. Not hermes_cli.main,
# which reads .env as it is imported (before the restore).
import gateway.run  # noqa: E402,F401
import hermes_cli.gateway  # noqa: E402,F401
import gateway.relay.adapter  # noqa: E402,F401
import gateway.relay.ws_transport  # noqa: E402,F401

# Bounded by the boot: hermes-boot either says go or the container stops.
while not os.path.exists(GO):
    time.sleep(0.01)

# The environment the main program would have from with-contenv, then the
# gateway's own (its Relay's URL, id, and this boot's secret).
env_dir = "/run/s6/container_environment"
if os.path.isdir(env_dir):
    for k in os.listdir(env_dir):
        try:
            with open(os.path.join(env_dir, k)) as f:
                # s6 writes each value with a trailing newline
                os.environ[k] = f.read().removesuffix("\n")
        except OSError:
            pass
with open(os.path.join(RUN, "gateway.env")) as f:
    for line in f:
        k, _, v = line.rstrip("\n").partition("=")
        if k:
            os.environ[k] = v
home = os.environ.get("HERMES_HOME", "/data/hermes")
os.environ.update(HOME=home, HERMES_HOME=home)
os.chdir(home)

import hermes_cli.main  # noqa: E402

# The gateway hashes every bundled skill twice more at start (S3b, cut h2);
# when stage2's stamp says this image already synced them into this home,
# those walks are skipped. Any surprise keeps Hermes' own behavior.
try:
    rev = open("/opt/fragment/image-rev").read().strip()
    manifest = hashlib.sha256(open(os.path.join(home, "skills/.bundled_manifest"), "rb").read()).hexdigest()[:16]
    if open(os.path.join(home, ".fragment-stamps/skills")).read().strip() == f"{rev}:{manifest}":
        import tools.skills_sync

        tools.skills_sync.sync_skills = lambda quiet=False: {"copied": [], "updated": [], "skipped": 0, "user_modified": [], "cleaned": [], "suppressed": [], "total_bundled": 0, "optional_provenance_backfilled": []}
        hermes_cli.main._sync_bundled_skills_quietly = lambda: None
except (OSError, AttributeError):
    pass

sys.argv = ["hermes", "gateway", "run"]
sys.exit(hermes_cli.main.main())
