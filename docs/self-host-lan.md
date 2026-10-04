# fragment on the home network, run as an intranet

Status: **built 2026-10-04, tested on high ports from this box** (branch
`selfhost`, the spike; never merged). The steps marked **sudo** and
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

State lives in `FRAGMENT_LAN_STATE` (the guide uses
`~/.local/state/fragment-lan`, so `cargo clean` never takes the CA the
devices trust):

- `ca/`: the root (`ca.key`, 0600; `ca.pem`, `fragment-ca.crt`,
  `fragment-ca.mobileconfig`) and the zone's certificate (`zone.pem`,
  `zone.key`), issued fresh at each start for 397 days (iOS allows 825);
- `dex/`: `dex.yaml` (0600), the client's secret, and `passwords/<name>`
  (0600), one per person;
- `serve.json`: the front door's config.

Logs: `target/devstack/lan-door.log` (a device that does not trust the
root yet shows there as `TLS from <addr> failed: … UnknownCA` or
`BadCertificate`), `target/devstack/lan-dex.log`, and the node log the
banner names.

## 1. The box, once (sudo)

Run these from the spike's checkout, `~/dev/finite/fragment-selfhost`.

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
`https://dex.fragment.home.arpa`, so the box asks its own DNS for the zone
(and the router for everything else, as now):

```sh
sudo mkdir -p /etc/systemd/resolved.conf.d
printf '[Resolve]\nDNS=192.168.50.7\nDomains=~fragment.home.arpa\n' | sudo tee /etc/systemd/resolved.conf.d/fragment-lan.conf
sudo systemctl restart systemd-resolved
```

Check it once the stack runs (step 2): `resolvectl query
dex.fragment.home.arpa` answers 192.168.50.7.

**1e. Optional: the box's browser.** Chromium on the box trusts the root
with no sudo: `certutil -d sql:$HOME/.pki/nssdb -A -t C,, -n 'fragment LAN
CA' -i ~/.local/state/fragment-lan/ca/ca.pem` (after step 2 makes it).

## 2. Start the stack

```sh
cd ~/dev/finite/fragment-selfhost
export FRAGMENT_LAN_STATE=$HOME/.local/state/fragment-lan
export FRAGMENT_LAN_BIN=/usr/local/lib/fragment-lan/fragment-lan   # step 1b's (omit with the sysctl)
export CELLD_BIN=$HOME/dev/finite/celld/target/release/celld
export FRAGMENT_MODEL_URL=http://bonsai.localhost/v1
export FRAGMENT_MODELS='{"@cf/zai-org/glm-5.3":"bonsai-2-27b","@cf/zai-org/glm-5.3-flash":"bonsai-2-27b"}'
cargo xtask dev --lan
```

The first start makes the CA and fetches Dex. The banner says where
everything is: the front door, the DNS server and its upstream, the root's
page with its **SHA-256 fingerprint** (write it down: step 4 compares it),
and each person with their password's file. Ctrl-C stops all of it.

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
it).

From the box, once 1d is done:

```sh
resolvectl query todo--paul.fragment.home.arpa              # 192.168.50.7
curl --cacert $FRAGMENT_LAN_STATE/ca/ca.pem -sI https://fragment.home.arpa/ | head -1   # HTTP/1.1 200 OK
```

## 3. Point the iPhone at the box's DNS

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

## 4. Trust the root on the iPhone

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

## 5. The Mac

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
```

Or open the profile (`http://192.168.50.7/ca`) and install it in System
Settings, General, Device Management. Firefox keeps its own store: in
`about:config`, `security.enterprise_roots.enabled` true.

**The CLI**, built from this branch (it trusts the OS's roots beside the
public ones, as master will with PR #130; the released CLI trusts the public
roots alone):

```sh
cargo build --release -p fragment-cli && cp target/release/fragment ~/.local/bin/
export FRAGMENT_HOST=https://fragment.home.arpa
fragment login        # approve the key in the browser, signed in through Dex
fragment whoami
```

**Its sandcastle node, later** (the Mac runner): it dials the box, so the
Mac needs no open port. In its config, `"uplink": {"url":
"wss://fragment.home.arpa/api/nodes/uplink", "id": "mac"}`, `"platform":
"https://fragment.home.arpa"` and `"ca_file": "<the fragment-ca PEM>"`. On
the box, a row in the node list (step 8): `{"id": "mac", "uplink": true,
"arch": "aarch64", "capacity": 8, "secret_file": "<its secret>"}`, with
the images' arm64 references (docs/self-host.md, seam 2, Placement). The
uplink through the front door was tested (below).

## 6. Sign in from the iPhone

1. Safari: `https://fragment.home.arpa` (type the `https://`; a bare name
   may go to a search). The padlock is the root's.
2. Sign in. Dex asks for an email and a password: `paul@fragment.home.arpa`
   and the password from step 2. Let iCloud Keychain keep it.
3. The first time, choose a username (`paul`): your fragments are then
   `https://<label>--paul.fragment.home.arpa`.
4. Make a fragment from the shell's catalog, or with the CLI from the Mac
   or the box. Talk to an agent: its model is Bonsai on the box.

Add the shell to the home screen (Share, Add to Home Screen) for an app of
its own. Push notifications need Apple's push service and the internet;
offline, the shell's live channel tells an open tab instead
(docs/self-host.md, seam 8).

## 7. The WAN-unplugged test

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
   ss -tnp state established '( not dst 192.168.50.0/24 and not dst 127.0.0.0/8 and not dst [::1] )' | grep -E 'celld|dex|fragment-lan|ninfer'
   ```

   prints nothing. The door's log shows the DNS forwards that failed (other
   names, now unreachable): those are devices' lookups, not the stack's.
5. Expected gaps, each a known debt: the shell's fonts (Google Fonts) fall
   back to the system's (docs/self-host.md, seam 11); an agent's skills that
   fetch from npm or a CDN fail; push is off.
6. Plug the WAN back in (and Cellular Data back on).

## 8. Computers on this box

The local sandcastle node (`~/.local/opt/sandcastle/node.json`, on
127.0.0.1:8798) joins through a node list, `FRAGMENT_NODES_FILE`
(docs/self-host.md, seam 2, Placement):

```sh
cat > ~/.local/opt/sandcastle/nodes.json <<'JSON'
{ "nodes": [
    { "id": "box", "url": "http://127.0.0.1:8798", "arch": "x86_64", "capacity": 32,
      "secret_file": "/home/futurepaul/.local/opt/sandcastle/node.secret" } ],
  "images": { "stub": "<the reference the node holds>" } }
JSON
export FRAGMENT_NODES_FILE=$HOME/.local/opt/sandcastle/nodes.json
```

before `cargo xtask dev --lan`. The node's own `platform` is the front door,
`https://fragment.home.arpa`, with `"ca_file"` the root's PEM. The platform
takes a node's intercepts (`/api/nodes/egress`) on its own host alone, so
the cell's loopback port is not enough. The Mac's row (step 5) joins the
same list, and computers are placed on either. Today its engine's VMs die before they are
ready (the engine fix waits on its restart, which needs root); until then a
computer's wake fails and says so, and nothing else changes. A computer's
own origin is `https://<id>--computer.fragment.home.arpa`, under the same
certificate.

## 9. Undo it all

```sh
# the box
sudo rm /etc/systemd/resolved.conf.d/fragment-lan.conf && sudo systemctl restart systemd-resolved
sudo ufw status numbered          # then, for each fragment-lan rule, highest number first:
sudo ufw delete <n>
sudo rm -r /usr/local/lib/fragment-lan            # 1b, setcap's way
sudo rm /etc/sysctl.d/50-fragment-lan.conf && sudo sysctl -w net.ipv4.ip_unprivileged_port_start=1024   # 1b, the sysctl's way
certutil -d sql:$HOME/.pki/nssdb -D -n 'fragment LAN CA'                  # 1e
rm -r ~/.local/state/fragment-lan                 # the CA, Dex's secrets: every device's trust in it ends here
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
| Safari: "This Connection Is Not Private" | the root is installed but not trusted: step 4.3. The door's log says `UnknownCA` or `BadCertificate` for the phone's address |
| Safari: "Safari can't find the server" | DNS: step 3's server (`192.168.50.7` alone); Private Relay; the stack is not running |
| `fragment-lan serve` exited: permission denied on :53, :80 or :443 | step 1b; with setcap, `FRAGMENT_LAN_BIN` must name the installed copy |
| xtask: `… says "", not "fragment-lan 1"` | the installed `fragment-lan` is older than this checkout: step 1b's install and setcap again |
| `fragment-lan`: the address is in use | another server holds 53, 80 or 443 on 192.168.50.7 |
| Sign-in: "the sign-in provider's metadata did not answer" | the box cannot resolve the zone (step 1d), or Dex is down (`lan-dex.log`) |
| Dex: "Invalid Email Address and password" | the email is `<name>@fragment.home.arpa`; the password is the file's |
| the CLI: "unreachable, or it dropped the connection" | its machine does not trust the root yet (step 5), or cannot resolve the zone |
| "the cell is not answering" (502) | the stack is starting or stopped; see the node log |

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
