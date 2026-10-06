# fragment on the home network, run as an intranet

Status: **built 2026-10-04; rehearsed end to end 2026-10-05** from this box
on high ports, short of sudo, the router and the devices (Evidence, below;
branch `selfhost`, the spike, never merged). The steps marked **sudo** and
the router's and the phone's are Paul's: this guide is them, in order.
docs/self-host.md is the design (seams 4 and 9, "What corporate networks
bring"); this is how the home network plays a company's.

## What a company's IT would do, and what stands in for it

| A company's IT | Here |
|---|---|
| delegates a subzone to the team's server | `fragment.home.arpa` (RFC 8375's home domain), answered by DNS on the box (192.168.50.7); every other name goes on to the router |
| issues certificates from its CA, and pushes the root to every device | a CA made on the box, **constrained to `fragment.home.arpa`** (it can vouch for nothing else, even if its key leaked); its root installed on the iPhone and the Mac by profile |
| puts one TLS edge in front of the apps | `fragment-lan`'s front door on :443: `https://fragment.home.arpa`, fragments at `https://<label>--<user>.fragment.home.arpa`, Dex at `https://dex.fragment.home.arpa`; WebSockets and event streams pass through |
| runs an identity provider | Dex v2.45.1 (pinned by digest), one static user per person, passwords in files |
| runs the model on its own network | Bonsai (`http://bonsai.localhost/v1`) |
| runs the workloads on its own machines | sandcastle's engine on the box (microVMs, jailed) behind a `sandcastle-node`; a person's own machine (the Mac) paired to them |
| opens the firewall to the office subnet only | ufw: 53, 80 and 443 from 192.168.50.0/24 |

## What runs on the box

`cargo xtask dev --lan` (xtask/src/lan.rs) runs the dev stack on celld and,
beside it:

| Process | Listens | What |
|---|---|---|
| `fragment-lan serve` (crates/lan) | 192.168.50.7: 53 (UDP and TCP), 80, 443 | DNS for the zone; the front door (TLS, HTTP/1.1, upgrades); the root's page over http |
| Dex | 127.0.0.1:8800 | the issuer `https://dex.fragment.home.arpa`, behind the door |
| celld | 127.0.0.1:8790 | the cell and the agents' Worker, on loopback: devices reach them only through the door |
| the code store, the model fake | 127.0.0.1:8792, :8796 | as `cargo xtask dev` (the model fake answers only image steps; Bonsai answers text) |
| the card renderer | 127.0.0.1, a free port | preview cards: the pinned chrome-headless-shell, which reaches fragments through the door and trusts the root |

and in a second terminal, `sandcastle-node serve` on 127.0.0.1:8798, in
front of the engine (`/var/lib/sandcastle`): the box's computers.

State lives in `FRAGMENT_LAN_STATE` (this guide uses
`~/.local/state/fragment-lan`, so `cargo clean` never takes the CA the
devices trust):

- `ca/`: the root (`ca.key`, 0600; `ca.pem`, `fragment-ca.crt`,
  `fragment-ca.mobileconfig`) and the zone's certificate (`zone.pem`,
  `zone.key`), issued fresh at each start for 397 days (iOS allows 825);
- `dex/`: `dex.yaml` (0600), the client's secret, and `passwords/<name>`
  (0600), one per person;
- `serve.json`: the front door's config;
- `nodes.json` and `node.json`: the node list and the box's node (step 2,
  written by you).

Logs: `target/devstack/lan-door.log` (a device that does not trust the
root yet shows there as `TLS from <addr> failed: … UnknownCA` or
`BadCertificate`), `target/devstack/lan-dex.log`, and the node log the
banner names.

## 1. The box, once (sudo)

Run these from the spike's checkout, up to date:

```sh
cd ~/dev/finite/fragment-selfhost && git pull --ff-only
```

**1a. A fixed address.** The zone answers 192.168.50.7, so the box must
keep it. On the ASUS router (http://192.168.50.1): LAN, DHCP Server,
"Manually Assigned IP around the DHCP list": Enable, then add the box
(omarchy) with 192.168.50.7, and Apply.

**1b. The privileged ports.** Only `fragment-lan` binds 53, 80 and 443.
Give that one binary, at a path only root can write, the right to:

```sh
cargo build --release -p fragment-lan
sudo install -D -o root -g root -m 0755 target/release/fragment-lan /usr/local/lib/fragment-lan/fragment-lan
sudo setcap cap_net_bind_service=+ep /usr/local/lib/fragment-lan/fragment-lan
getcap /usr/local/lib/fragment-lan/fragment-lan   # cap_net_bind_service=ep
```

The other way: `echo 'net.ipv4.ip_unprivileged_port_start=53' | sudo tee
/etc/sysctl.d/50-fragment-lan.conf && sudo sysctl --system`.

Tradeoff: setcap lets only that root-owned binary take low ports, but each
new build of it needs the install again (xtask refuses a stale one and says
so); the sysctl needs no reinstall, but lets any program of any user on the
box take 53 to 1023.

**1c. The firewall,** to the home subnet only:

```sh
sudo ufw allow from 192.168.50.0/24 to any port 53 comment 'fragment-lan DNS'
sudo ufw allow from 192.168.50.0/24 to any port 80 proto tcp comment 'fragment-lan root page'
sudo ufw allow from 192.168.50.0/24 to any port 443 proto tcp comment 'fragment-lan front door'
sudo ufw status numbered
```

**1d. The box's own resolver.** The cell fetches Dex at
`https://dex.fragment.home.arpa`, and the box's node calls the front door,
so the box asks its own DNS for the zone (and the router for everything
else, as now):

```sh
sudo mkdir -p /etc/systemd/resolved.conf.d
printf '[Resolve]\nDNS=192.168.50.7\nDomains=~fragment.home.arpa\n' | sudo tee /etc/systemd/resolved.conf.d/fragment-lan.conf
sudo systemctl restart systemd-resolved
```

Check it once the stack runs (step 3): `resolvectl query
dex.fragment.home.arpa` answers 192.168.50.7.

**1e. Optional: the box's browser.** Chromium on the box trusts the root
with no sudo, once step 3 has made it: `mkdir -p ~/.pki/nssdb && certutil
-d sql:$HOME/.pki/nssdb -A -t C,, -n 'fragment LAN CA' -i
~/.local/state/fragment-lan/ca/ca.pem` (restart Chromium after).

## 2. Computers on this box, once (no sudo)

New computers run Hermes (our image) in microVMs on this box's engine,
through a `sandcastle-node` in front of it (docs/self-host.md, seam 2,
Placement). Three things, once; run `node-images` again after each `git
pull` that changes `images/` (a computer takes the new image at its next
wake):

```sh
cd ~/dev/finite/fragment-selfhost
export FRAGMENT_LAN_STATE=$HOME/.local/state/fragment-lan
mkdir -p -m 700 $FRAGMENT_LAN_STATE
# 1. the stub and Hermes, built here and loaded into the engine (Docker: your user
#    in the docker group); a few minutes, most of it Hermes' build
cargo xtask node-images
# 2. the node list: this box's node, and the images node-images printed
cat > $FRAGMENT_LAN_STATE/nodes.json <<JSON
{ "nodes": [
    { "id": "box", "url": "http://127.0.0.1:8798", "arch": "x86_64", "capacity": 16,
      "secret_file": "$HOME/.local/opt/sandcastle/node.secret" } ],
  "images": { "stub": "docker.io/library/fragment-stub:local", "hermes": "docker.io/library/fragment-hermes:local" } }
JSON
# 3. the node's config: its computers' requests go to the front door, under the root
cat > $FRAGMENT_LAN_STATE/node.json <<JSON
{ "listen": "127.0.0.1:8798",
  "engine": "/var/lib/sandcastle/engine.sock", "ports": "/var/lib/sandcastle/ports.sock",
  "egress": "$XDG_RUNTIME_DIR/fragment-lan-node/egress.sock",
  "secret_file": "$HOME/.local/opt/sandcastle/node.secret",
  "platform": "https://fragment.home.arpa", "ca_file": "$FRAGMENT_LAN_STATE/ca/ca.pem" }
JSON
```

The heredocs fill in `$HOME` and the rest as they write. The node's secret
is S2's, `~/.local/opt/sandcastle/node.secret` (if it is not there:
`(umask 077; openssl rand -hex 32 > ~/.local/opt/sandcastle/node.secret)`).
Leave `~/.local/opt/sandcastle/node.json` as it is: it is the dev stack's
(`platform` its loopback port), and this one is the LAN's. The node's
`platform` must be the front door, not the cell's port: the platform takes
a node's intercepts (`/api/nodes/egress`) on its own host alone. The
engine takes Hermes' image (1.36 GB as `docker save` writes it) in about
25 s the first time, as it builds its disk; again, at once. Its capacity
is the engine's `vms_max` (16 here).

## 3. Start it

**Terminal 1, the stack:**

```sh
cd ~/dev/finite/fragment-selfhost
export FRAGMENT_LAN_STATE=$HOME/.local/state/fragment-lan
export FRAGMENT_LAN_BIN=/usr/local/lib/fragment-lan/fragment-lan   # step 1b's (omit with the sysctl)
export FRAGMENT_LAN_USERS=paul,mac-test   # the people; mac-test tries the Mac (step 7)
export CELLD_BIN=$HOME/dev/finite/celld/target/release/celld
export FRAGMENT_MODEL_URL=http://bonsai.localhost/v1
export FRAGMENT_MODELS='{"@cf/zai-org/glm-5.3":"bonsai-2-27b","@cf/zai-org/glm-5.3-flash":"bonsai-2-27b"}'
export FRAGMENT_NODES_FILE=$FRAGMENT_LAN_STATE/nodes.json   # step 2's list
export FRAGMENT_COMPUTER_IMAGE=hermes   # new computers run Hermes, its model Bonsai
export FRAGMENT_BYOC=on                 # people pair machines of their own (the Mac, step 7); a company leaves it off
cargo xtask dev --lan
```

**Terminal 2, the box's node,** once terminal 1 prints its banner (the
first start makes the root the node trusts):

```sh
~/.local/opt/sandcastle/bin/sandcastle-node serve --config ~/.local/state/fragment-lan/node.json
```

It says `serving on 127.0.0.1:8798, intercepts on … to
https://fragment.home.arpa`. Check that Bonsai answers too: `curl -s
http://bonsai.localhost/v1/models` names `bonsai-2-27b`.

The first start makes the CA and fetches Dex. The banner says where
everything is: the front door, the DNS server and its upstream, the root's
page with its **SHA-256 fingerprint** (write it down: step 5 compares it),
and each person with their password's file.

**Stopping:** Ctrl-C in each terminal. The stack's takes a second, or up
to 25 s while a computer is awake (celld lets its calls to the node drain),
and frees every port. Computers that were awake keep running on the engine
while the stack is down; the next start finds them, and they sleep when
idle. To see them: `curl -s --unix-socket /var/lib/sandcastle/engine.sock
http://engine/v1/containers`.

The people are `FRAGMENT_LAN_USERS` (default `paul`; a comma-separated
list). Each signs in as `<name>@fragment.home.arpa` with the password in
`$FRAGMENT_LAN_STATE/dex/passwords/<name>`: made on first start as
`xxxx-xxxx-xxxx-xxxx-xxxx`, lower-case letters and digits, easy to type on
a phone. Read it on the box with `cat`. To choose your own instead, write it
there before the first start (12 characters or more):

```sh
mkdir -p -m 700 $FRAGMENT_LAN_STATE/dex/passwords
( umask 077; read -rs p && printf '%s' "$p" > $FRAGMENT_LAN_STATE/dex/passwords/paul )
```

Every setting, with its default: `FRAGMENT_LAN_ZONE` (fragment.home.arpa),
`FRAGMENT_LAN_ADDR` (the box's address toward its default gateway),
`FRAGMENT_LAN_BIND` (that address), `FRAGMENT_LAN_DNS_UPSTREAM` (the
gateway, :53), `FRAGMENT_LAN_HTTPS_PORT` (443), `FRAGMENT_LAN_HTTP_PORT` (80,
0 for none), `FRAGMENT_LAN_DNS_PORT` (53, 0 for none), `DEX_BIN` (the pinned
Dex), and `FRAGMENT_DEV_PORT` (8790: the cell, and its fakes and Dex above
it). Preview cards reach fragments through the door (its first address, on
the https port) and trust the root, unless `FRAGMENT_BROWSER_UPSTREAM` or
`FRAGMENT_BROWSER_CA_FILE` say otherwise.

From the box, once 1d is done:

```sh
resolvectl query todo--paul.fragment.home.arpa              # 192.168.50.7
curl --cacert $FRAGMENT_LAN_STATE/ca/ca.pem -s -o /dev/null -w '%{http_code}\n' https://fragment.home.arpa/   # 200
```

(`curl -I` asks with HEAD, which the shell answers 404: use the line above.)

## 4. Point the iPhone at the box's DNS

**On the iPhone alone** (start here): Settings, Wi-Fi, the (i) beside the
home network, Configure DNS, **Manual**. Delete the servers listed, Add
Server, `192.168.50.7`, Save.

**Or for the whole house, on the router**: the ASUS's LAN, DHCP Server,
"DNS and WINS Server Setting": DNS Server 1 `192.168.50.7`, DNS Server 2
empty, "Advertise router's IP in addition to user-specified DNS" **No**,
Apply. Devices take it at their next lease (toggle Wi-Fi to hurry one).
Every device then depends on the box for all its DNS: while the box is
off, the house has none. Leave DNS Server 2 empty: a device asks either
server, and the router does not know the zone.

With Asuswrt-Merlin, the router can stay everyone's DNS and forward only
the zone to the box, as a company delegates a subzone. Administration,
System, "Enable JFFS custom scripts and configs": Yes; then over ssh:

```sh
printf 'server=/fragment.home.arpa/192.168.50.7\nrebind-domain-ok=/fragment.home.arpa/\n' >> /jffs/configs/dnsmasq.conf.add
service restart_dnsmasq
```

**iCloud Private Relay** sends Safari's lookups past the network's DNS. If
Safari cannot find `fragment.home.arpa`, turn it off for this network:
Settings, Wi-Fi, (i), "iCloud Private Relay" off.

## 5. Trust the root on the iPhone

1. In **Safari** (profiles download only there), open
   `http://192.168.50.7/ca`. Tap "iPhone, iPad or Mac: the profile", then
   Allow. "Profile Downloaded".
2. Settings, General, **VPN & Device Management**, "fragment on
   fragment.home.arpa" under Downloaded Profile. Tap More Details, then the
   certificate: its SHA-256 fingerprint must be the one the box's banner
   printed (the page over http could have been swapped on the way; the
   banner could not). Back, **Install**, your passcode, Install, Install
   (iOS warns of an unmanaged root), Done.
3. Settings, General, About, **Certificate Trust Settings** (at the bottom).
   Under "Enable full trust for root certificates", turn on "fragment LAN
   CA (omarchy)", Continue.

The root says, inside it, that it is valid for `fragment.home.arpa` and the
names under it only: the phone will not trust it for any other site.

## 6. Sign in from the iPhone, and talk to Hermes

1. Safari: `https://fragment.home.arpa` (type the `https://`; a bare name
   may go to a search). The padlock is the root's. "Agents that work for
   you, and the apps they make": **Sign in**.
2. Dex asks for an email and a password: `paul@fragment.home.arpa` and the
   password from step 3. Let iCloud Keychain keep it.
3. **Choose a username** (`paul`): your fragments are then
   `https://<label>--paul.fragment.home.arpa`. The shell then makes your
   first agent by itself, with a name it picks ("Creating your agent…
   Starting its computer"), and opens its chat. Your computer is a Hermes
   microVM on the box (step 2); its model is Bonsai.
4. **Talk to it.** A plain answer takes about 1 to 6 s, a turn with a
   terminal command about 9 s, each tool a step under the reply ("Worked
   through 1 step"). A computer left idle sleeps; your next message wakes
   it.
   - **When it asks you something, answer in the chat**: your next message
     is the answer, while its turn waits (an open question, or "Other" on
     a card of choices: tap it, then type). Stop works while it waits.
   - Ask it to keep files under `/data/hermes` (say `/data/hermes/notes/`):
     it may write nowhere else, and it will say so.
5. **New agent**: the **+** at the top left. Give it a job (it becomes its
   `SOUL.md`) and a name, Make it. Its chat opens with the job as your
   first message, and it answers on the computer you already have.
6. **An app**: open the sidebar (top left), the **+** beside Apps, and
   **Make it** under Todo (rename it first if you like). It opens in a
   window; the sidebar shows its preview card once it is shot.
7. **Live**: in a second Safari tab, open
   `https://todo--paul.fragment.home.arpa` ("you and 1 other here"). Add a
   todo in one; it shows in the other at once.

Add the shell to the home screen (Share, Add to Home Screen) for an app of
its own. Push notifications need Apple's push service and the internet;
offline, the shell's live channel tells an open tab instead
(docs/self-host.md, seam 8). A computer made before step 2 runs the stub;
the shell shows an update pill that moves it to Hermes.

## 7. The Mac

**DNS, for the zone only** (the rest stays as it is; this is split DNS, as
a company's VPN does it):

```sh
sudo mkdir -p /etc/resolver
echo 'nameserver 192.168.50.7' | sudo tee /etc/resolver/fragment.home.arpa
dscacheutil -q host -a name dex.fragment.home.arpa   # ip_address: 192.168.50.7
```

(`dig` and `nslookup` skip `/etc/resolver`; `dscacheutil`, Safari and
everything else use it.)

**The root**, for Safari, Chrome, `curl` and the `fragment` CLI:

```sh
curl -sO http://192.168.50.7/ca/fragment-ca.crt
openssl x509 -inform der -in fragment-ca.crt -noout -fingerprint -sha256   # the banner's
sudo security add-trusted-cert -d -r trustRoot -k /Library/Keychains/System.keychain fragment-ca.crt
curl -so home-ca.pem http://192.168.50.7/ca/ca.pem    # the same root as PEM, for the Mac's Linux VM (below)
```

Or open the profile (`http://192.168.50.7/ca`) and install it in System
Settings, General, Device Management. Firefox keeps its own store: in
`about:config`, `security.enterprise_roots.enabled` true.

**The CLI**, built from this branch or master (it trusts the OS's roots
beside the public ones since master's PR #130; a CLI released before that
trusts the public roots alone):

```sh
cargo build --release -p fragment-cli && cp target/release/fragment ~/.local/bin/
export FRAGMENT_HOST=https://fragment.home.arpa
fragment login        # it opens the link; approve the key, signed in through Dex
fragment whoami
```

**Its sandcastle node: pair it** (the Mac runner; experimental:
docs/self-host.md, seam 2, Bring your own computer). It dials the box, so
the Mac needs no open port, and it becomes your node, for your computers
alone. With sandcastle's engine running in the Mac's Linux VM (sandcastle's
docs/mac.md, steps 1 to 6, and its "The CA" with `home-ca.pem` from above):

1. The images, in the VM (with Docker and Rust), from a checkout of this
   branch: `cargo xtask node-images`. It builds them for arm64 there and loads them into the
   VM's engine under the same names as the box's list (`…:local`), so the
   list needs no change. (The arm64 Hermes image has not been built whole
   yet; its stages have.)
2. Pair, in the VM:

   ```sh
   sudo install -d -m 0700 -o "$(id -un)" /etc/sandcastle-node   # once: the node (you, not root) writes its config and secret here
   sandcastle-node pair https://fragment.home.arpa --config /etc/sandcastle-node/node.json --name mac \
     --ca-file /etc/sandcastle/home-ca.pem \
     --engine /var/lib/sandcastle/engine.sock --ports /var/lib/sandcastle/ports.sock \
     --egress /run/sandcastle-node/egress.sock
   ```

   It prints a link and a code.
3. **Who it is for.** A computer is placed once, at its first start, and
   stays (moving is not built), and the shell starts your first one as
   you pick a username. Yours already runs on the box, so try the Mac with
   the second person, `mac-test` (step 3's `FRAGMENT_LAN_USERS`): on the
   Mac, in a private window, open the link, sign in as
   `mac-test@fragment.home.arpa` (its password is
   `$FRAGMENT_LAN_STATE/dex/passwords/mac-test` on the box), check the page
   shows the same code, tick **Run my new computers on it**, and tap **Add
   this node**. The command ends having written
   `/etc/sandcastle-node/node.json` (the platform, its uplink as the id the
   box gave it, `ca_file`) and the secret (`node.secret`, 0600; never
   shown).
4. Start it with docs/mac.md's unit (its `RuntimeDirectory` makes
   `/run/sandcastle-node`), on `/etc/sandcastle-node/node.json`, and wait
   for its log's `uplink: connected`. **Before step 5**: a first computer
   whose chosen node is down goes to the box instead, for good.
5. "Node added" says "Your new computers run on it": **Open the shell**,
   choose a username, and mac-test's first agent starts on the Mac.
   Settings, Computers (experimental) shows the box, and `mac` (yours, Up,
   "Your computer runs here"). Revoke it there, and it is cut off at once.

You pair a node for yourself the same way; the tick only matters for a
person with no computer yet (yours stays on the box).

## 8. The WAN-unplugged test

It shows the whole loop needs nothing outside the house.

1. Everything working (steps 1 to 6). On the iPhone, turn **Cellular Data
   off**: otherwise iOS sends traffic over cellular once Wi-Fi has no
   internet, and the test proves nothing.
2. Unplug the router's WAN cable (or: WAN, Internet Connection, "Enable
   WAN" No).
3. On the iPhone: sign out and sign in again through Dex; open a fragment;
   change something and watch it update live in a second tab; ask an agent
   something and get Bonsai's answer.
4. On the box, nothing of the stack holds a connection past the house:

   ```sh
   ss -tnp state established '( not dst 192.168.50.0/24 and not dst 127.0.0.0/8 and not dst [::1] )' | grep -E 'celld|dex|fragment-lan|sandcastle|ninfer'
   ```

   prints nothing. The door's log shows the DNS forwards that failed (other
   names, now unreachable): those are devices' lookups, not the stack's.
5. Expected gaps, each a known debt: the shell's fonts (Google Fonts) fall
   back to the system's (docs/self-host.md, seam 11); an agent's skills that
   fetch from npm or a CDN fail; push is off.
6. Plug the WAN back in (and Cellular Data back on).

## 9. Undo it all

```sh
# the box
sudo rm /etc/systemd/resolved.conf.d/fragment-lan.conf && sudo systemctl restart systemd-resolved
sudo ufw status numbered          # then, for each fragment-lan rule, highest number first:
sudo ufw delete <n>
sudo rm -r /usr/local/lib/fragment-lan            # 1b, setcap's way
sudo rm /etc/sysctl.d/50-fragment-lan.conf && sudo sysctl -w net.ipv4.ip_unprivileged_port_start=1024   # 1b, the sysctl's way
certutil -d sql:$HOME/.pki/nssdb -D -n 'fragment LAN CA'                  # 1e
rm -r ~/.local/state/fragment-lan                 # the CA, Dex's secrets, step 2's files: every device's trust in it ends here
# the Mac
sudo rm /etc/resolver/fragment.home.arpa
sudo security delete-certificate -c 'fragment LAN CA (omarchy)' /Library/Keychains/System.keychain
```

- The iPhone: Settings, Wi-Fi, (i), Configure DNS, **Automatic**; Settings,
  General, VPN & Device Management, the profile, **Remove Profile** (that
  removes the root and its trust).
- The router: DNS Server 1 empty and "Advertise router's IP" Yes (or remove
  Merlin's two lines and `service restart_dnsmasq`); the box's manual DHCP
  assignment, if it is no longer wanted.

## When something fails

| What you see | Why, and what to do |
|---|---|
| Safari: "This Connection Is Not Private" | the root is installed but not trusted: step 5.3. The door's log says `UnknownCA` or `BadCertificate` for the phone's address |
| Safari: "Safari can't find the server" | DNS: step 4's server (`192.168.50.7` alone); Private Relay; the stack is not running |
| `fragment-lan serve` exited: permission denied on :53, :80 or :443 | step 1b; with setcap, `FRAGMENT_LAN_BIN` must name the installed copy |
| xtask: `… says "", not "fragment-lan 1"` | the installed `fragment-lan` is older than this checkout: step 1b's install and setcap again |
| `fragment-lan`: the address is in use | another server holds 53, 80 or 443 on 192.168.50.7 |
| xtask: `read …/nodes.json (FRAGMENT_NODES_FILE)` | step 2's list is not written yet |
| `sandcastle-node: ca_file: … No such file or directory` | terminal 2 started before the stack's first start made the root: start the stack, then the node |
| a new agent's chat says its computer could not start, or `node_down` | terminal 2's node is not running, or its `platform` is not the front door (step 2) |
| Sign-in: "the sign-in provider's metadata did not answer" | the box cannot resolve the zone (step 1d), or Dex is down (`lan-dex.log`) |
| Dex: "Invalid Email Address and password" | the email is `<name>@fragment.home.arpa`; the password is the file's |
| an app's row shows its icon, never its card | the card renderer could not reach the door: see the stack's output for `card browser:` lines |
| the CLI: "unreachable, or it dropped the connection" | its machine does not trust the root yet (step 7), or cannot resolve the zone |
| "the cell is not answering" (502) | the stack is starting or stopped; see the node log |

## Evidence, 2026-10-05: the guide rehearsed

From a worktree of `selfhost` (`../fragment-rehearse`, branch
`selfhost-rehearse`) rather than `~/dev/finite/fragment-selfhost`, with
the state, the node list and the node's config in its scratch rather than
`~/.local/state/fragment-lan`. In place of Paul's steps:

- **1b and 1c**: high ports, `FRAGMENT_LAN_HTTPS_PORT=9543
  FRAGMENT_LAN_HTTP_PORT=9580 FRAGMENT_LAN_DNS_PORT=9553`, the door on
  192.168.50.7 alone (the default bind).
- **1d**: every process of the stack, both nodes and the CLI ran in a mount
  namespace whose `/etc/hosts` mapped `fragment.home.arpa` and
  `dex.fragment.home.arpa` to 192.168.50.7 (`unshare -rm`, then an inner
  user namespace mapping back to uid 1000: Chrome's sandbox refuses root).
- **The iPhone**: headless Chromium 152 at 390×844 (3×), touch, Safari's
  user agent, its own NSS database trusting the root (step 1e's
  `certutil`), and `--host-resolver-rules` for the zone.
- **The Mac**: sandcastle's Docker engine double and `sandcastle-node`
  built from sandcastle's `node` (cf61527), in the same namespace.

The real engine (`/var/lib/sandcastle`, jailed microVMs) ran the box's
computers, Bonsai-2-27B answered, and the engine was empty before and
after. Screenshots: `target/rehearse/shots/` in that worktree.

| Step | What happened |
|---|---|
| 2 | `node-images`: 15 s with Docker's cache; 183 s after a change to the bridge (Hermes' load 23.6 s, 22.3 s of it its disk) |
| 3 | the stack ready 1.5 s after its build (the first build of a fresh checkout, cell and agents, about 50 s; chrome-headless-shell and Dex fetched once); the node in front of the engine |
| 6.1–6.3 | the shell, signed in through Dex's form; a username; the first agent's chat open 2.3 s after Continue, its computer awake on the engine |
| 6.4 | "What is 17 times 23?": 391, the turn over in 5.6 s; "run `uname -a` and `nproc`": one terminal step, "an x86_64 Linux machine (kernel 6.12.91) with 2 CPUs", 9.0 s |
| 6.5 | New agent, made in 0.6 s, its chat open with the job; Hermes' first answer began 13.6 s after Make it (the computer warm); another with three tool steps answered in 41 s |
| 6.4, asking | an open clarify asked in 5.5 s; the answer typed in the chat ended the turn 10.6 s later, the file written; Stop while it asked ended the turn in 0.4 s; "Other" on a card of choices, then a typed answer: done in 1.8 s |
| 6.6–6.7 | Todo from the catalog, its window at once; a todo added in one tab showed in the other 54 ms later, "you and 1 other here"; its card shot in 1.2 s |
| 7 | `fragment login` approved through the door, `whoami` paul; `sandcastle-node pair` printed the link and code, mac-test signed in through Dex from the link, ticked the box and added the node; the config and its 0600 secret written; the uplink up; mac-test's first agent placed on the Mac ("as its owner chose"), and it ran `hostname` there in 9.7 s; Settings, Computers: "Your computer runs here" |
| 3, stopping | Ctrl-C freed every port, in 1 s once and 19 s once (celld's drain, one call to the node in flight); each node stopped on Ctrl-C; the engine double removed its containers |

**What the rehearsal found, and what changed** (each fixed here; the first
two are on master too, with PRs of their own):

- **An agent asking in words deadlocked its chat** (found 26's "Other",
  and more). Hermes' `clarify` with no choices sends `❓ <question>` and
  waits for the person's next message; the bridge showed the question as a
  step and queued the next message as a turn behind the asking one, which
  waited an hour (Hermes' clarify timeout), and Stop could not end it. The
  first agent Juniper did exactly this on its first message. Now the
  bridge shows the question as the agent's reply, hands the asker's next
  message to the running turn (`Command::Tell`; Hermes' clarify intercept
  takes it), and answers a Stop's interrupt with "Stop." so the wait lets
  go. "Other" on a card works the same way.
- **`fragment login` on Linux waited for the browser it opened** to exit
  (`xdg-open` runs a browser in the foreground when none is running), so it
  never finished after the approval. It no longer waits for the opener.
- **Preview cards on the LAN** were never shot: the renderer reached
  fragments at 127.0.0.1 on the door's port, where the door does not
  listen, and did not trust the root. On the LAN both default to the
  door's (its first address, and the root).
- **The Mac could never hold a second person's computer**: the shell
  starts a person's first computer as they pick a username, before
  settings, where the choice was. The pairing page now offers "Run my new
  computers on it", which chooses the node as it is added.
- **The guide**: step 2 exported `FRAGMENT_NODES_FILE` before step 8 wrote
  it (the stack refused to start); the node's config was never written
  down (its `platform` must be the front door, with `ca_file`), and the
  node could not start before the stack's first start made the root; the
  first agent is made at the username, not by New agent; `curl -I` is the
  shell's 404; stale lines about VMs dying and master's PR #130.

Not rehearsed: ports 53, 80 and 443 (sudo), systemd-resolved, ufw, the
router, the iPhone's Safari itself (its profile, Private Relay, the home
screen), the Mac's Linux VM and the arm64 images, and the WAN unplugged.

## Evidence, 2026-10-04 (this box, high ports, no sudo)

Run as `FRAGMENT_LAN_HTTPS_PORT=9543 FRAGMENT_LAN_HTTP_PORT=9580
FRAGMENT_LAN_DNS_PORT=9553 FRAGMENT_LAN_BIND=127.0.0.1,192.168.50.7
FRAGMENT_DEV_PORT=9500 cargo xtask dev --lan`, on celld with Bonsai, inside a
mount namespace (`unshare -rm`) whose `/etc/hosts` mapped the zone's names
to 192.168.50.7 (what step 1d does for the box). The stack was ready in
4.6 s.

- **DNS** (`dig @127.0.0.1 -p 9553`, and on 192.168.50.7): the apex,
  `todo--paul.…` and `DEX.Fragment.Home.Arpa` answered `192.168.50.7`,
  NOERROR with `aa`; AAAA was NODATA with the zone's SOA; NS and SOA at the
  apex; `example.com` and `github.com` came back from the router, over UDP
  and TCP; a name that does not exist was the router's NXDOMAIN.
- **TLS** (`curl --resolve … --cacert ca.pem`): the shell 200, Dex's
  discovery naming `https://dex.fragment.home.arpa:9543`; without the root,
  curl refused the certificate; a Host outside the zone, 421. `openssl
  verify` passed the zone's certificate, and showed the root's critical
  name constraint (`DNS:fragment.home.arpa`), the SAN (the zone and
  `*.fragment.home.arpa`), serverAuth and 397 days.
- **The root's page** at `http://192.168.50.7:9580/ca`: the profile as
  `application/x-apple-aspen-config`; a zone name over http moved (308) to
  https with its path and query.
- **Sign-in through Dex, by curl**: `/auth/login` went to Dex's form; a
  wrong password was Dex's 401; the password, read from its file, came back
  to `/auth/callback`, which set `__Host-fragment_session` (Secure: the cell
  took https from the door's `x-forwarded-proto`). The identity was
  `(https://dex.fragment.home.arpa:9543, <sub>)` with the email and the
  handle `paul` (Dex's `preferredUsername`). A second sign-in was the same
  person; a second user another; a username was claimed; a form from an
  `http://` origin was refused (403); logout ended the session.
- **The CLI** (`SSL_CERT_FILE` naming the root, as the OS store would):
  `fragment login` approved its key through the door, `whoami` answered
  paul, `init --template todo` deployed `https://todo-lan--paul.…:9543/`.
  Without the root, it could not connect.
- **Headless Chromium** (its NSS store trusting the root, the zone mapped by
  `--host-resolver-rules`): Dex's pages and the fragment were "secure",
  issued by "fragment LAN CA (omarchy)"; it signed in through Dex's form;
  the owner's fragment opened on its own origin; a second tab with the share
  link said "you and 1 other here"; a todo added in one tab appeared in the
  other, over `wss://todo-lan--paul.fragment.home.arpa:9543/__live`.
- **An agent on Bonsai**: `fragment agent create` and `fragment agent say`
  through the door; Bonsai-2-27b answered "Paris is the capital of France."
  in 0.8 s.
- **The uplink**: the real `sandcastle-node` with `ca_file` and `"uplink":
  {"url": "wss://fragment.home.arpa:9543/api/nodes/uplink"}` connected
  through the door, the platform's signed hello verified.
- **Ctrl-C** (SIGINT to the stack's process group) stopped xtask, Dex, the
  door and celld, and freed every port. Before, `xtask dev --runtime celld`
  left celld running in a group of its own, holding its port; now dev keeps
  it in the terminal's. A start that fails (a stale `fragment-lan`, a port in
  use) leaves nothing running.
- **Without step 1d** the box cannot resolve Dex, and sign-in says so: "the
  sign-in provider's metadata did not answer".
- Host tests (`crates/lan`, `xtask`): 31, for the zone's answers and the
  forwarding (a silent router is SERVFAIL after 3 s), the certificates (a
  name outside the zone refused, even one the root signs), the door (Host
  routing, forwarded headers replaced, an event stream unbuffered, a
  WebSocket both ways, 502 for a dead upstream, the http page), Dex's
  config (secrets kept, ids stable across starts, a chosen password), and
  the settings.

Not tested here: ports 53, 80 and 443 themselves (sudo), the iPhone and the
Mac, the router, and the WAN-unplugged run. Those are steps 1 to 7.

## Design notes

- **One process holds the privileged ports**, and it parses only DNS, TLS
  and HTTP heads; it holds no secret but the zone's key.
- **The root is constrained to the zone.** An installed root is otherwise
  trusted for every site; with RFC 5280 name constraints, a leak of this
  box's key reaches nothing but `fragment.home.arpa`.
- **The zone's certificate is a wildcard** for its one level: every
  fragment, Dex and a computer share it, and nothing pins it, so it is
  issued fresh each start. A company that bans wildcards gives one
  certificate per name, or path mode (docs/self-host.md, seam 9).
- **The cell is the same cell**: its zone (`FRAGMENT_HOST_SUFFIX`), its
  origin (`FRAGMENT_PLATFORM_URL`, https), its issuer, and a root for its
  own fetches (`CELLD_EXTRA_CA_FILE`) are configuration. It takes the
  scheme from `x-forwarded-proto`, as it did behind Fly's proxy; the door
  replaces that header and `x-forwarded-host` on every request, and celld
  listens on loopback only.
- **DNS forwards, it does not resolve.** The router's answers reach the
  device unchanged; a company's resolver would forward this zone to the box
  (Merlin's two lines are exactly that).
- **The CLI trusts the OS's roots** beside the public ones, as every client
  on a company's network must. The e2e and the cell still trust the public
  roots alone.
