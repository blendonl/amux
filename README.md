# amux

A terminal multiplexer written in Rust.

## Usage

```sh
cargo run --                             # start the server if needed, then attach to the most recent session or create one here
cargo run -- new                         # create a session; inside a git repo it runs in this branch's worktree
cargo run -- new -s work                 # create a session named "work"
cargo run -- new -s work --on laptop     # create it on the server named laptop instead
cargo run -- new -p amux -b feature-x    # a session in the feature-x worktree of the amux project
cargo run -- new -p amux --on laptop --clone  # on laptop, cloning amux there first if it has no checkout
cargo run -- attach -t work              # reattach (aliases: a, attach-session)
cargo run -- attach -t work@laptop       # attach to a session on another server
cargo run -- rename -t work@laptop notes # rename a session (alias: rename-session)
cargo run -- kill -t notes@laptop        # kill a session (alias: kill-session)
cargo run -- kill -t amux/feature-x --remove-worktree  # kill it and delete its worktree
cargo run -- ls                          # list the sessions on every server in the cluster
cargo run -- ls --by project             # the same sessions, grouped by project
cargo run -- projects                    # every project and where it is checked out
cargo run -- project add [path]          # register the repo at path (default: here) with this server
cargo run -- servers                     # status, latency and version of every server
cargo run -- servers add laptop ssh://laptop  # link to laptop and remember it in servers.lua
cargo run -- servers remove laptop
cargo run -- servers forget laptop       # drop laptop, its key and its addresses, and refuse that key from now on
cargo run -- discover                    # what Tailscale and the LAN found, and how linking each machine goes
cargo run -- pair                        # open a pairing window on this machine and print a one-time code
cargo run -- pair k7-4821-9930           # on the other machine: pair with the one that printed the code
cargo run -- pair k7-4821-9930 --host 192.168.0.24:40123  # the same, when multicast does not reach it
cargo run -- pair --new-key              # pair with a new key, so a forgotten machine can come back
cargo run -- config check                # load init.lua and servers.lua and say which files were read
cargo run -- config defaults             # every default setting, as Lua for a starting init.lua
cargo run -- kill-server                 # stop the server and all sessions
```

Inside a session, press `Ctrl-b d` to detach and `Ctrl-b Ctrl-b` to send a literal `Ctrl-b`. The other keys after `Ctrl-b` manage windows and panes, as in tmux, and `Ctrl-b s` opens a tree of every session in the cluster. The bottom row is a status bar that shows the session, its server and its windows.

`-L <name>` picks a named server socket in the runtime directory and `-S <path>` sets an explicit socket path. Both work like tmux's flags, so you can run an isolated dev server next to your usual one. Each `-L` name is its own cluster: a `-L dev` server only links to other `-L dev` servers.

### Windows and panes

A session holds numbered windows, and each window splits into panes, each running its own shell. The host lays the panes out and draws them into one screen with borders between them, and the border around the active pane is green.

| Keys                    | Action                                           |
| ----------------------- | ------------------------------------------------ |
| `Ctrl-b c`              | New window                                       |
| `Ctrl-b n`, `Ctrl-b p`  | Next and previous window                         |
| `Ctrl-b 0` … `Ctrl-b 9` | The window with that number                      |
| `Ctrl-b %`              | Split the active pane into left and right panes  |
| `Ctrl-b "`              | Split the active pane into top and bottom panes  |
| `Ctrl-b o`              | Next pane                                        |
| `Ctrl-b` arrow key      | The pane in that direction                       |
| `Ctrl-b x`              | Kill the active pane                             |
| `Ctrl-b &`              | Kill the active window                           |
| `Ctrl-b ,`              | Rename the active window                         |
| `Ctrl-b $`              | Rename the session                               |
| `Ctrl-b s`              | Pick a session or window anywhere in the cluster |
| `Ctrl-b d`              | Detach                                           |
| `Ctrl-b Ctrl-b`         | Send a literal `Ctrl-b`                          |

- Windows are numbered from 0. A new window takes the lowest free number, and the others keep theirs when one closes. `amux ls` counts the windows of every session in the cluster.
- A split shares the pane's space equally with its siblings, and every shell is resized to its pane. A split that leaves no room for the new pane is refused, and the status bar says why.
- A pane whose shell exits leaves the layout and its neighbours take over its space. A window closes with its last pane, and the session ends with its last window.
- Every client attached to a session sees the same active window and pane. The window takes the size of the client that typed last, and a client with a smaller terminal sees the top-left part of it.
- While a window has more than one pane, the terminal reports mouse clicks to amux, and clicking a pane makes it the active one. Most terminals still select text when you hold Shift. A program that turns on mouse reporting itself, like `vim` with `mouse=a` or `htop`, gets the clicks inside its own pane in its own coordinates.
- These keys work the same on a session on another server: they travel over the peer link to the server that holds the session.

### Status bar and the cluster tree

The client keeps the bottom row of the terminal for a status bar, so a session gets one row less than the terminal has:

```
[notes@laptop] 0:sh  1:vim                            home-server offline  12 ms
```

- `[session@server]` is the attached session and the server that holds it.
- The windows follow, with the active one highlighted. When they don't fit, the list is cut around the active window and `…` marks the hidden ones.
- On the right are the servers that are offline and, for a session on another server, the latency of the link to it. On a narrow terminal the offline servers shrink to a count and then go away before any window does.
- When something you asked for fails, such as a split with no room, a rename to a name that is taken or a switch to a session that has just gone, the status bar shows the error for three seconds. The client stays attached.

`Ctrl-b s` opens the cluster tree over the session: every server, then its projects, then their sessions and windows, starting on the session you are in.

```
- desktop  (this server)
  - amux
    + amux/main  2 windows  attached
  - (no project)
    + scratch  1 window
- laptop  12 ms
  - notes
    + notes/main  1 window
```

`j` and `k` or the arrow keys move, `l` and `h` expand and collapse, `g` and `G` jump to the top and the bottom, and `Enter` picks. `q`, `Escape` and `Ctrl-c` close the tree. Picking a session or a window switches this client to it, wherever it runs, without leaving the client. The sessions of a server that is offline are dimmed as stale and can't be picked.

`Ctrl-b ,` and `Ctrl-b $` open a prompt on the status bar, filled in with the window's or the session's current name. The arrow keys, `Home`, `End`, `Ctrl-a`, `Ctrl-e`, `Backspace`, `Delete` and `Ctrl-u` edit it, `Enter` renames and `Escape` or `Ctrl-c` cancels. Renaming a session works the same when it runs on another server.

While the tree or a prompt is open, keys and mouse clicks go to it and never reach the session. A lone `Escape` closes it after 50 ms, since it could also be the start of an arrow key.

### Targets

`-t` takes a target of the form `[session][@server][:window[.pane]]`, like tmux's `-t` with a server added:

| Target            | Means                                                         |
| ----------------- | ------------------------------------------------------------- |
| `work`            | The session named `work`, which must be unique in the cluster |
| `work@laptop`     | The session `work` on `laptop`                                |
| `@laptop`         | The most recently active session on `laptop`                  |
| `work:1`          | Window 1 of `work`                                            |
| `work@laptop:1.0` | Pane 0 of window 1 of `work` on `laptop`                      |
| (none)            | The most recently active session on this server               |

When a name matches sessions on several servers, the command fails and lists them all, for example `session work is on more than one server, pick one of: work@desktop, work@laptop`. Session names can't contain `@` or `:`, because targets use them as separators.

Panes are numbered from 0 in layout order. `attach -t work:1.0` makes window 1 and its pane 0 active before attaching. `kill -t work:1` kills only window 1, and `kill -t work:1.0` only that pane, so a session loses nothing else. A window or pane that doesn't exist is an error that names it.

### Config

amux is configured in Lua. The server loads the config when it starts, and so do `amux servers add`, `amux servers remove` and `amux config`. It runs the first of these that applies:

1. The file named by `--config <path>` or `AMUX_CONFIG`, which must exist
2. `$XDG_CONFIG_HOME/amux/init.lua`, usually `~/.config/amux/init.lua`
3. `/etc/amux/init.lua`, which amux only ever reads
4. None, which means the built-in defaults

Your `init.lua` replaces the system file rather than adding to it. To build on the system file, start yours with `dofile("/etc/amux/init.lua")`. `AMUX_CONFIG=/dev/null` runs amux with the defaults. `require` finds modules in the `lua/` directory next to `init.lua`.

```lua
local opt = amux.opt

opt.name = "desktop"
opt.projects_dir = "~/projects"

opt.servers.laptop = {
  address = "ssh://laptop",
  amux_path = "~/.cargo/bin/amux",
  socket = "default",
}

opt.projects.amux = {
  default_server = "desktop",
  worktrees_dir = "~/projects/amux-worktrees",
}

opt.discovery.tailscale_tags = { "tag:server" }
opt.lan.port = 7448
```

Every option has a built-in default, so a config only sets what it changes. `amux.opt` is strict: an unknown option or a value of the wrong type is an error that names the file and line, such as `init.lua:3: unknown option amux.opt.bogus, expected one of …`. Values that would break the server, such as a zero interval or a blank `name`, are errors too. A bad config stops the server before it starts listening, so a typo is never ignored.

| Command                | Does                                                                                        |
| ---------------------- | ------------------------------------------------------------------------------------------- |
| `amux config check`    | Loads the config as the server and as the client, then prints the files it read             |
| `amux config defaults` | Prints every default as assignments to `amux.opt`, which works as a starting `init.lua`     |
| `amux config path`     | Prints where your `init.lua` is or goes: the `--config` file, or the one in the config dir  |

`name` defaults to the hostname and `projects_dir` to `~/projects`. A leading `~` in `projects_dir` and `worktrees_dir` means your home directory. Each entry under `servers` is a peer to link to. Each entry under `projects` is keyed by project name: `default_server` is where `amux new` puts that project's sessions when you don't pass `--on`, and `worktrees_dir` is where its worktrees go instead of the default `<checkout>/../<project>-worktrees`.

`amux.opt.discovery` controls how servers find each other without a config entry (see [Cluster](#cluster)):

| Option           | Default                                    | Means                                                                 |
| ---------------- | ------------------------------------------ | --------------------------------------------------------------------- |
| `tailscale`      | `true`                                     | Link to the machines on the tailnet and listen on the tailnet address |
| `lan`            | `true`                                     | Find servers over mDNS, listen on the LAN and allow `amux pair`       |
| `tailscale_tags` | `{}`                                       | Tailscale tags whose machines count as yours, such as `"tag:server"`  |
| `tailscale_port` | 7447, or a port derived from the `-L` name | The port of the tailnet listener, the same on every machine           |

`amux.opt.lan.port` is the port of the LAN listener. The default, `0`, takes a free port each time the listener opens, which mDNS then advertises. A fixed port helps with firewall rules and with `amux pair --host` without a port.

A server's `address` is one of:

- `ssh://[user@]host[:port]`, which runs `ssh -T -o BatchMode=yes -o ServerAliveInterval=15 [-p port] [user@]host <amux_path> -L <socket> bridge`. `amux_path` is passed to the remote shell as written, so `~` works. Without it, the remote command is a short `sh -c` script that looks for amux on the remote `PATH` and in the usual install locations (see [Over SSH](#over-ssh-configured-by-hand)). `socket` defaults to this server's own socket name.
- `exec:<command>`, which runs the command directly, split like a shell would split it but without a shell. The command has to end in `amux … bridge`. The tests use it to link servers on one machine.
- `tcp://host:port`, which links over Noise to that address. The other server's key must already be trusted, or vouched for by the tailnet. Tailscale discovery uses this form.
- `lan://<server id>`, the form LAN discovery uses. The IP addresses and port come from mDNS each time amux dials.

#### servers.lua

`amux servers add` and `amux servers remove` never edit `init.lua`. They rewrite `servers.lua`, which amux owns and which always sits in the config directory: next to the `--config` file when you pass one, otherwise in `$XDG_CONFIG_HOME/amux`. It holds data only, a table of servers that can't call functions or read globals:

```lua
return {
  laptop = {
    address = "ssh://laptop",
  },
}
```

amux loads `servers.lua` before `init.lua` and merges its entries into `amux.opt.servers`, so `init.lua` can read them, change them or add more. Assigning a whole table, as in `amux.opt.servers = { … }`, replaces all of them, including the ones from `servers.lua`. Set one server at a time instead: `amux.opt.servers.laptop = { … }`.

A rewrite goes to a temporary file first, and replaces `servers.lua` only when the whole config still loads with it. `servers add` refuses a name that `init.lua` or `servers.lua` already has, and `servers remove` refuses a server that only `init.lua` sets and tells you to remove it there. Both then tell a running server about the change. `amux servers forget` removes the server from `servers.lua` too. When `init.lua` also sets it, the server is still forgotten, and amux says it is still in `init.lua`.

Each server also has a random ID, stored in `$XDG_STATE_HOME/amux/<socket name>/server-id`, and a fresh incarnation ID every time it starts. The log shows both. Its Noise key and the keys it trusts live next to the ID (see [Trust](#trust)).

### Upgrading

Every connection starts with a greeting that carries the protocol version. When the client and the running server speak different major versions, the client names both and asks you to run `amux kill-server`. `kill-server` also works on a server that is too old to answer the greeting.

This version speaks protocol 7, which added the Noise keys, trust updates and discovery, so every machine in the cluster needs the new build. Servers on different major versions refuse to link, and `amux servers` lists the older one as `incompatible, runs amux … (protocol 6.0)`. An older build doesn't listen on TCP, so Tailscale and LAN discovery can't reach it either. The cluster cache of the older build still loads.

amux no longer reads `config.toml`. Move its settings into `init.lua` as assignments to `amux.opt`: `name = "desktop"` becomes `amux.opt.name = "desktop"`, a `[servers.laptop]` table with `address = "ssh://laptop"` becomes `amux.opt.servers.laptop = { address = "ssh://laptop" }`, and `tailscale = false` under `[discovery]` becomes `amux.opt.discovery.tailscale = false`. Then run `amux config check`, and restart the server with `amux kill-server` so it loads the new file.

### Cluster

Every server keeps one link to each server it knows about, so the servers form a full mesh. `amux ls` answers from a cache of every peer's sessions and never waits on the network:

```
desktop (this server)
  amux/main              3 windows   attached
  amux/feature-x         1 window
laptop                   12 ms
  notes/main             1 window
home-server              offline, last seen 3h ago
  infra/main             2 windows   stale
```

- A server dials every address in its config, every address discovery finds on the tailnet and the LAN, and every address its peers have linked over, retrying with a backoff from 1 s up to a minute while a peer is away. Adding one machine on one server is enough for the rest to find it, as long as its address works from every machine. Peers never pass on the addresses discovery found, since each machine discovers for itself, nor `tcp://` addresses on loopback, which only work on one machine. When one address comes from several places, the local config wins over discovery and discovery wins over what peers report. A learned or discovered address that isn't seen for a week is forgotten.
- When both sides dial at once, the link dialed by the lower server ID survives. A server that restarts replaces its old link at once. A second server that claims a name another online server already uses is refused until it is renamed.
- `kill-server` sticks. A stopping server says goodbye on every link, and its peers mark it stopped and only dial it with `amux bridge --no-start`, so they never start it again behind your back. It comes back when you start it.
- Peers, their addresses, the stopped flag and the last known sessions are kept in `$XDG_STATE_HOME/amux/<socket name>/cluster-cache`, so an offline server still shows its last known sessions after a restart. A cache that fails to decode is dropped.
- Links send a ping every 5 seconds and drop a peer that stays silent for three of them. `AMUX_PING_INTERVAL_MS` changes the interval, which the tests use to notice a lost link quickly.

Two machines link in one of three ways:

| Where                | What you do                            | How they link                         |
| -------------------- | -------------------------------------- | ------------------------------------- |
| On the same tailnet  | Nothing                                | Noise over TCP on the tailnet address |
| On the same LAN      | `amux pair` once for each new machine  | Noise over TCP, found over mDNS       |
| Anywhere SSH reaches | `amux servers add laptop ssh://laptop` | `ssh laptop amux bridge`              |

However a machine joins, the rest of the cluster learns its key and links to it wherever it can reach it: on the tailnet, on the LAN or through an address a peer passes on.

Discovery only links servers that are already running, because nothing starts a server on another machine over TCP. A server that `amux bridge` starts over SSH inherits a non-interactive SSH environment and may be killed by logind when the SSH session ends. To keep a machine reachable either way, run its server from a systemd user unit (`ExecStart=%h/.cargo/bin/amux server`) and allow it to outlive your login with `loginctl enable-linger`.

#### On the same tailnet

Servers on one tailnet link on their own. There is nothing to configure, no SSH and no keys to copy, because the tailnet vouches for each machine.

- Every 30 seconds a server reads `tailscale status --json` and dials the peers that are online, run Linux, macOS, FreeBSD or OpenBSD, and either belong to the same Tailscale user as this machine or carry a tag listed in `amux.opt.discovery.tailscale_tags`. It dials them at `tcp://<tailnet IPv4>:<port>`. Phones, sleeping machines and other users' machines are never dialed.
- A server listens on its own tailnet IPv4 address and nowhere else, at port 7447 for the default socket. Any other `-L` name gets a port between 7448 and 7947 derived from the name, so servers with the same `-L` name find each other. `amux.opt.discovery.tailscale_port` sets the port. A server dials its peers at its own port setting, so give every machine the same one. The listener follows the tailnet address as it appears, changes or goes away, and a server dials its tailnet peers from that address.
- The first time two servers meet, neither trusts the other's key, so each asks `tailscale whois` about the other's tailnet address: the dialing side about the address it dialed, the accepting side about the address the connection came from. The tailnet vouches for a machine of the same user, or one with an allowed tag, as long as it isn't this machine's own node. Each side then trusts the other's key first hand and needs no whois for it again. If whois fails, for example because `tailscaled` is down, the link is refused, so an address in 100.64.0.0/10 that isn't on the tailnet is never trusted.
- Tailscale gives every tagged machine the same owner, so a tagged machine accepts other machines by tag only. Two tagged machines link when each lists the other's tag in `tailscale_tags`. A tagged and an untagged machine never link on their own, because the untagged one carries no tag the tagged one could accept. Pair them on the LAN or link them over SSH instead.
- A machine that goes offline or leaves the tailnet is not dialed again, through any address discovery found or a peer passed on, until Tailscale lists it as online again. Addresses in the config are still dialed.
- A tailnet machine shows up in `amux servers`, `amux ls` and the status bar only once amux has reached an amux server there. Until then only `amux discover` lists it, and its retries back off up to 10 minutes.

`amux.opt.discovery.tailscale = false` turns this off. When Tailscale isn't installed, the server logs it once and stops looking. `AMUX_DISCOVERY_INTERVAL_MS` changes how often it reads the status, which the tests use to see changes quickly.

#### On the LAN, with a pairing code

Servers on one LAN find each other over mDNS. A new machine needs a one-time pairing code, and after that it links over Noise with nothing more to do.

Run `amux pair` on one machine. It prints a code and waits in the foreground:

```
$ amux pair
pairing merges this machine's cluster with the other machine's, and gives each machine full access to the other, like ssh as this user
pairing code k7-4821-9930, valid for 5 minutes and one use
run this on the other machine:
  amux pair k7-4821-9930
or, where multicast does not reach it:
  amux pair k7-4821-9930 --host <this machine's address>:40123
waiting for the other machine...
paired with laptop (5be0c7a1f29d4e8b93a6d10c7e42f851), key fingerprint 3f9a:1c07:88d2:e4b1
```

The first part of the code, `k7`, names the pairing window, which the server advertises over mDNS while it is open. The eight digits are the secret. `amux pair k7-4821-9930` on the other machine finds the server that advertises `k7`, pairs with it, and prints the same warning and a `paired with` line naming the first machine.

- Where multicast doesn't get through, as on many guest and office networks, add `--host <ip[:port]>` with the first machine's IP address and the port `amux pair` printed. It takes an IP address, not a host name. Without a port, amux uses the port that mDNS saw for that address, or this machine's own `amux.opt.lan.port` if it is set. The joining machine doesn't need LAN discovery for this.
- A window is good for one pairing and five minutes. It also closes after three attempts or when you leave `amux pair`, and only one `amux pair` can wait on a server at a time. One pairing connection runs at a time, and an attempt counts as soon as its first pairing message arrives. A wrong code fails on the machine that tried it, and the waiting machine prints how many attempts are left.
- The code itself never crosses the network. The two machines open a Noise connection and run SPAKE2 inside it, bound to both machines' keys and to that connection's handshake, so the exchange only succeeds between the two machines that hold the code.
- Pairing merges two clusters. The two machines trust each other first hand, the pairing connection becomes their first link, and trust spreads from there (see [Trust](#trust)): every machine linked to either one learns the keys of the other cluster and links to those machines wherever it can reach them. That is what the warning at the top is about.
- Afterwards the two find each other over mDNS whenever they share a LAN, even when DHCP moves them. Their address is `lan://<server id>`, and amux looks up the current IP addresses and port each time it dials.
- `amux pair --new-key`, with or without a code, gives this server a new key before it pairs. That is how a forgotten machine comes back.

A server browses for `_amux._tcp.local.` on every interface except loopback and interfaces named `tailscale*`, `utun*`, `docker*`, `br-*` and `veth*`. It advertises itself only while its LAN listener is open (see [Listening ports](#listening-ports)), with its server ID as the instance name and its name, key, `-L` socket name, protocol version and any open pairing window in the TXT record. Servers with another `-L` name are ignored. `amux.opt.discovery.lan = false` turns off LAN discovery and the LAN listener, and `amux pair` then only works on the joining side, with `--host`.

#### Over SSH, configured by hand

`amux servers add laptop ssh://laptop` links to any machine that key-based SSH reaches, on the LAN, on the tailnet or across the internet. On the other machine, `amux bridge` connects stdin and stdout to the local server and starts that server if it isn't running. SSH handles auth and encryption, and `~/.ssh/config` applies as usual. `BatchMode=yes` means an unknown host key or a passphrase prompt fails the link, and the reason shows in the server log.

A non-interactive SSH shell often has no `~/.cargo/bin` on its `PATH`, so without `amux_path` the remote command looks for amux: `amux` on the `PATH`, then `~/.cargo/bin/amux`, `~/.local/bin/amux`, `/usr/local/bin/amux` and `/opt/homebrew/bin/amux`. When none of them is there, the link fails with "amux is not installed on this machine". Set `amux_path` when amux lives anywhere else.

Both servers record each other's key on the first SSH link, and the cluster learns it, so members that reach that machine on the tailnet or the LAN can link to it over Noise without SSH.

#### Trust

Cluster membership means full trust: any member can open a shell on any other, as the user that runs amux there, just like SSH to that machine. So nothing links without authentication.

- Every server has a Noise key in `$XDG_STATE_HOME/amux/<socket name>/noise-key`, created with mode 0600 on first start, and sends its public key in its hello. The keys it trusts are in `trust.toml` in the same directory, one key per server ID, together with the servers it has forgotten.
- A server trusts a key first hand when it sees the key itself: over an SSH or `exec:` link, from a tailnet peer that `tailscale whois` vouched for, or by pairing. A key seen first hand replaces an older key for that server.
- Trust spreads through the cluster. Linked servers send each other their trust store when the link comes up and whenever it changes. A key learned this way records its introducer, the member that saw it first hand, and never replaces a key the server already knows. Once a machine joins through one member, every member trusts it and can link to it over Noise.
- A Noise link must present the key the trust store holds for the server ID in its hello, unless the tailnet or a pairing vouches for it. A server refuses an unknown key right after the Noise handshake, before it sends its own hello.

`amux servers forget <name or id>` takes a machine out of the cluster for good:

- It drops the link, the server's record, every address of it, its entry in `servers.lua` and its key. An entry in `init.lua` stays, and amux tells you to remove it there.
- It records a tombstone for that server ID and key and sends it through the cluster, so every member forgets the machine too and closes its links to it.
- The keys the forgotten server introduced go as well, unless a member saw them first hand.
- From then on the server is refused on every transport and discovery skips it. This is permanent for that key. To bring the machine back, run `amux pair --new-key` on it and pair it with any member: it gets a new key and rejoins the whole cluster, and its old key stays out.

A known limitation: a member that trusted the old key second hand and missed the tombstone, for example because it was offline when you ran `servers forget`, won't accept the new key until the member that paired with the machine sends it another trust update.

#### Why a machine doesn't show up

`amux discover` lists what Tailscale and the LAN found and how linking each machine goes. When a machine is missing from `amux servers`, look here:

```
tailscale      running with 2 machines
  desk         tcp://100.101.7.12:7447                  linked
  nas          tcp://100.88.20.4:7447                   failing: connecting to 100.88.20.4:7447: Connection refused (os error 111)
  work-laptop  tcp://100.97.3.8:7447                    absent
lan            running
  mini         lan://5be0c7a1f29d4e8b93a6d10c7e42f851   paired, linked
  pi           lan://c41e07b9d2a85f3e6b1c0a9d8e7f6a5b   not paired
```

Each source first says how it is doing: `off (disabled in the config)`, `not running`, `tailscale is not installed`, `running with 2 machines` (the tailnet machines it would dial right now), `running`, or the error that stopped it, such as `mDNS failed: …`. Under it, each machine it found has one of these statuses:

| Status             | Means                                                                                     |
| ------------------ | ----------------------------------------------------------------------------------------- |
| `linked`           | Linked right now                                                                          |
| `trying`           | amux is dialing it and no attempt has failed yet                                          |
| `failing: <error>` | The last attempt failed. `Connection refused` means no amux server listens there          |
| `absent`           | The source no longer lists it, for example because it is offline, so amux doesn't dial it |
| `not paired`       | A server on the LAN whose key this machine doesn't trust. Run `amux pair` to pair them    |
| `pairing open`     | A server on the LAN that is waiting in `amux pair`                                        |

LAN servers this machine trusts show `paired, ` before their status. A refusal shows as the error, for example when another server already uses the machine's name. A machine that has never linked or paired with anything doesn't advertise itself on the LAN, so it appears only once `amux pair` runs on it. `amux discover` needs a running server.

#### Listening ports

With both discovery sources off, amux opens no ports. Otherwise it listens on TCP in two cases, and only for other amux servers: clients always use the unix socket, and a TCP connection that greets as a client is refused.

| Listener | Address                                                         | Open while                                                                           | Turn it off                             |
| -------- | --------------------------------------------------------------- | ------------------------------------------------------------------------------------ | --------------------------------------- |
| Tailnet  | This machine's tailnet IPv4, port 7447 or `tailscale_port`      | Tailscale discovery runs and Tailscale is up with an IPv4 address for this machine   | `amux.opt.discovery.tailscale = false`  |
| LAN      | Every interface (`0.0.0.0`), `amux.opt.lan.port` or a free port | LAN discovery is on and the trust store holds a key or an `amux pair` window is open | `amux.opt.discovery.lan = false`        |

- The LAN listener writes its port to `$XDG_STATE_HOME/amux/<socket name>/lan-port` while it is open and advertises it over mDNS. It closes again when the trust store is empty and no window is open.
- LAN discovery also takes part in mDNS on UDP port 5353 whenever it is on, to browse for other servers, and answers for this server only while the LAN listener is open.
- Before a connection is authenticated, it has 10 seconds to finish its handshake, at most 16 such handshakes run at once and further connections are dropped, and one pairing connection runs at a time. A pairing connection that arrives with no window open is closed.

### Remote sessions

A client only ever talks to its local server. Attaching to a session on another server opens a channel over the peer link to that server, which treats the channel like any other attached client. `amux new --on laptop`, `attach`, `rename` and `kill` all work the same way whichever server holds the session.

- A session created on another server starts in that server's `$HOME`, because paths differ between machines. The client's `LANG`, `LC_*` and `COLORTERM` are applied to the new shell wherever it runs, in place of the server's own.
- When the link drops while you are attached, the session keeps running on its server, a box saying "reconnecting to laptop…" covers the screen and the status bar lists laptop as offline. Once the link is back you get a full redraw. `Ctrl-b d` still detaches in the meantime, and `Ctrl-b s` can switch to another session.
- If that server is stopped with `kill-server`, or restarts and loses the session, the client exits as it would for a local session.
- When several clients are attached to one session, the one that typed or resized last sets its size, like tmux's `window-size latest`.

### Projects and worktrees

A project is a git repository, known across the cluster by its normalized `origin` URL (`github.com/blendonl/amux`), or by its root commit when it has no remote. Its name is the checkout's directory name. Each server keeps a registry of the projects it has checked out in `$XDG_STATE_HOME/amux/<socket name>/projects.toml`, and every server sees every other server's checkouts, so `amux projects` shows where each project can run:

```
amux       github.com/blendonl/amux
  desktop  /home/me/projects/amux
  laptop   /home/me/src/amux
notes      4b825dc642cb6eb9a060e54bf8d69288fbee4904
  desktop  /home/me/notes
```

- `amux new` inside a repo turns the current directory into a project and branch and sends those, not a path, because paths differ between machines. The server resolves the project to its own checkout. The first session created inside a repo registers it, and `amux project add [path]` registers one by hand. A detached `HEAD` gives a plain session in the current directory, and so does `amux new` outside a repo.
- `-p <name>` picks a project the cluster knows, by name or by id. When two different projects share a name, the command fails and lists their ids. `-b <branch>` picks the branch, and without it `-p` uses the project's default branch.
- The server is `--on` if given, then the project's `default_server`, then this server.
- There is one session per worktree. The session for the default branch runs in the main checkout. Any other branch gets a worktree in `worktrees_dir/<branch>`: an existing worktree for the branch is reused, an existing branch is checked out, a branch that only exists on `origin` is tracked, and a new branch starts from the default branch. When the branch doesn't exist locally, the server first fetches it from `origin`, with no prompts and a 30 second limit, so a branch pushed from another machine is found. `amux new` for a worktree that already has a session attaches to that session, even when several run at once.
- The session is named `<project>/<branch>`, with `@` and `:` turned into `-` and a `-2`, `-3`… suffix if the name is taken. `ls --by project` groups sessions this way across the cluster.
- When the server has no checkout, `amux new` fails and suggests `--clone` or running `amux project add` on that server. `--clone` makes that server clone the project from its `origin` into `projects_dir/<name>` and register it.
- Killing a session never deletes its worktree. `amux kill --remove-worktree` kills the session and then deletes its worktree. It refuses, and leaves the session running, when the worktree has uncommitted or untracked files or is the main checkout. The branch stays.

## Architecture

amux uses a client/server model like tmux. The server owns the shells and the client is only a view.

```
 terminal ── client ── unix socket ── server ─┬─ session ─┬─ window ─┬─ pane ── PTY ── $SHELL
  (raw mode)                            ║     │           │          └─ pane ── PTY ── $SHELL
                                        ║     │           └─ window ─── pane ── PTY ── $SHELL
                                        ║     └─ session ─── window ─── pane ── PTY ── $SHELL
                                        ║ peer link (ssh … amux bridge, or Noise over TCP)
                                        ╚══════════ server on another machine
```

- The **server** is started on demand as a detached `amux server` process. It listens on `$XDG_RUNTIME_DIR/amux-$UID/<name>` and logs to the same path with `.log` appended.
- Each **pane** spawns the user's shell in a PTY. Output goes through a `vt100` parser, so the server always holds the full screen state. That state is how a client gets a redraw when it reattaches. The shell doesn't inherit `SSH_*` variables from the server, and a session in a directory that doesn't exist fails instead of quietly starting in `$HOME`.
- A **session** holds an ordered list of windows, and each **window** holds a layout tree and its panes. The layout splits the window into rectangles with a one-cell border between siblings, and each pane's PTY is sized to its rectangle.
- The **compositor** paints the active window into a grid of cells: every pane's cells are read straight from its `vt100` screen while that pane's parser is locked, so nothing is copied per frame, and the borders are drawn with box-drawing characters. The cursor and input modes come from the active pane.
- Each attached **client** has its own differ, which compares the new grid with the last one that client was sent, clipped to that client's terminal size, and sends only the changed cells. It moves the cursor and clears line by line, never the whole screen. Frames are pulled, not pushed: output in a visible pane or a layout change only marks the client dirty, and the server composes a frame when the connection has room for one. A slow client skips intermediate screens, and its keystrokes never wait behind output. Keystrokes go back as raw bytes, key bindings as commands, and resizes are sent when `SIGWINCH` arrives.
- The host decodes **mouse** reports from the client's input. A click focuses the pane under the pointer, and a report only reaches a pane whose program asked for that kind of event, re-encoded in that pane's coordinates and format. A lone `Escape` that could start a report is held for at most 25 ms, so it never gets stuck.
- The **protocol** uses length-prefixed `postcard` frames. A connection opens with a `Greeting` and a `Welcome` whose layout never changes, then carries `ClientMessage` and `ServerMessage`. Any change to those messages bumps the major version. The server handles a connection as a `Duplex`, a pair of message channels, so it doesn't care what transport sits underneath.
- A **peer link** carries the same frames. After the greeting both servers send a `Hello`, the lower ID decides whether the link is a duplicate, and then each side sends a snapshot of its sessions followed by events stamped with its incarnation and a sequence number, so stale or repeated updates are dropped. One writer drains a control lane (pongs, credit, goodbyes, trust updates) ahead of a bulk lane (snapshots, events and channel data), and the reader never waits on anything the other side controls, so a peer that stops reading cannot stall this one.
- Over **TCP**, a connection opens with a `TcpOpen` frame that says whether it is a link or a pairing, then runs a `Noise_XX_25519_ChaChaPoly_BLAKE2s` handshake whose prologue binds that frame. A pump seals and opens the Noise messages between the socket and an in-memory duplex stream, so the peer link runs its usual framing on top. A pairing runs SPAKE2 over the same kind of connection first and then carries on as a link.
- A **channel** tunnels one client connection through a peer link. Each server numbers the channels it opens, and the server hosting the session runs the channel through the same connection handler as a local client, except that it only ever looks up its own sessions. The host sends at most four frames ahead and waits for the opening server to pass each one on to its client, so frames stay pulled end to end. Each channel has its own capped queue on the receiving side: a channel that overflows is closed on its own and the link stays up. The opening server forwards client messages without reading them, apart from detaching, switching sessions and listing the cluster, and reattaches by the host's incarnation and session ID when a dropped link comes back.
- The client draws its own **chrome**: the status bar, the reconnect overlay, the prompts and the cluster tree. It sends the host its terminal size without the status row, in the first request, on every resize and on a reattach, so the host never draws there. The host sends the session's name, windows and active window when the client attaches and whenever they change. The server the client is connected to adds a cluster status (its own name, the session's server, the link latency and the offline servers) on attach, whenever the cluster changes and every two seconds. A channel never carries a cluster status, since the server at the client's end knows it best. The status bar is drawn again after each batch of output, saving and restoring the cursor around it. While the tree is open the client drops the session's output, and closing the tree or a prompt asks the host for a full redraw.
- A client's terminal size is clamped to at least 2 rows by 2 columns, on the client and on the server, because the terminal emulator can't handle anything smaller.
- **Git** runs through the `git` CLI in blocking tasks, never while the server holds its sessions lock. Registry changes are serialized and saved before they are published as a `ProjectsChanged` event, and creates for the same project and branch wait on a per-worktree lock, so concurrent `amux new`s share one session.

| Path                         | Responsibility                                                                                                     |
| ---------------------------- | ------------------------------------------------------------------------------------------------------------------ |
| `src/main.rs`                | Parses the command line and dispatches                                                                             |
| `src/lib.rs`                 | The library the binary and the tests share                                                                         |
| `src/cli.rs`                 | Command-line interface                                                                                             |
| `src/paths.rs`               | Runtime, config and state paths, and the `init.lua` lookup order                                                   |
| `src/config/`                | `amux config check`, `defaults` and `path`, and the checked, atomic rewrites of `servers.lua`                      |
| `src/identity.rs`            | Server ID, incarnation and hostname                                                                                |
| `src/settings/`              | Every setting with its built-in default, as one `Settings` tree                                                    |
| `src/lua/`                   | The Lua runtime: the strict `amux.opt`, the `amux` API, data files like `servers.lua` and the Lua writer           |
| `src/target.rs`              | `session@server` targets: parsing, validation and resolution                                                       |
| `src/project/id.rs`          | Project ids from origin URLs, local paths and root commits                                                         |
| `src/project/detect.rs`      | Finding the project, branch and main checkout of a directory                                                       |
| `src/project/registry.rs`    | The `projects.toml` registry of local checkouts                                                                    |
| `src/project/worktree.rs`    | Default branch, worktrees, fetch, clone and removal                                                                |
| `src/project/git.rs`         | Running `git` without prompts, with an optional timeout                                                            |
| `src/protocol/mod.rs`        | Framing and the `Duplex` message channels                                                                          |
| `src/protocol/greeting.rs`   | Greeting, version constants and the version check                                                                  |
| `src/protocol/client.rs`     | Client and server messages                                                                                         |
| `src/protocol/peer.rs`       | Peer messages, snapshots and state events                                                                          |
| `src/protocol/key.rs`        | Public keys, their hex form and fingerprints                                                                       |
| `src/cluster/mod.rs`         | Membership, dial loops, peer cache                                                                                 |
| `src/cluster/link.rs`        | Peer handshake, link lanes, pings                                                                                  |
| `src/cluster/channel.rs`     | Channels over a link: ids, credit, capped inbound queues                                                           |
| `src/cluster/transport.rs`   | Addresses, how to dial each one, and how a link was authenticated                                                  |
| `src/cluster/ssh.rs`         | The SSH and exec transport, the remote amux lookup, `amux bridge`                                                  |
| `src/cluster/noise.rs`       | Noise keys, the TCP opening, the Noise handshake and the pump                                                      |
| `src/cluster/listener.rs`    | The tailnet and LAN listeners and the limits before authentication                                                 |
| `src/cluster/trust.rs`       | The `trust.toml` store and the rules for trust updates and tombstones                                              |
| `src/cluster/cache.rs`       | The cluster cache and reading older versions of it                                                                 |
| `src/discovery/mod.rs`       | Discovery sources, their state and the `amux discover` report                                                      |
| `src/discovery/tailscale.rs` | Reading `tailscale status`, the tailnet listener and `tailscale whois`                                             |
| `src/discovery/lan.rs`       | LAN candidates, the advertisement and `lan://` endpoints                                                           |
| `src/discovery/mdns.rs`      | Advertising and browsing over mDNS                                                                                 |
| `src/discovery/directory.rs` | A directory that stands in for mDNS in the tests                                                                   |
| `src/pairing.rs`             | Pairing codes and windows, and SPAKE2 inside Noise                                                                 |
| `src/server/mod.rs`          | Accept loop, session registry, state events, shutdown                                                              |
| `src/server/connection.rs`   | Per-client requests, target routing and the attach loop                                                            |
| `src/server/status.rs`       | The cluster status sent to attached clients                                                                        |
| `src/server/forward.rs`      | Forwarding a client to a session on another server, reconnects                                                     |
| `src/server/projects.rs`     | Project registry, checkouts, clones, worktree sessions and removal                                                 |
| `src/server/session.rs`      | Session state, its windows and the commands that change them                                                       |
| `src/server/window.rs`       | A window's layout, panes, active pane and mouse routing                                                            |
| `src/server/layout.rs`       | The pane layout tree: splits, rectangles, borders and neighbours                                                   |
| `src/server/render/`         | The compositor, the per-client differ and escape sequences                                                         |
| `src/server/mouse.rs`        | Decoding and re-encoding mouse reports                                                                             |
| `src/server/pane.rs`         | PTY, shell process, terminal emulation                                                                             |
| `src/client/mod.rs`          | Commands, server bootstrap, attaching                                                                              |
| `src/client/relay.rs`        | The attached client: keys, panels, chrome and switching sessions                                                   |
| `src/client/chrome/`         | Status bar, prompt, reconnect overlay, key decoding and drawing helpers                                            |
| `src/client/tree.rs`         | The `Ctrl-b s` cluster tree                                                                                        |
| `src/client/listing.rs`      | `amux ls`, `amux projects`, `amux servers`, `amux discover` and `amux pair` output                                 |
| `src/client/projects.rs`     | Resolving `-p` against the projects the cluster knows                                                              |
| `src/client/terminal.rs`     | Raw mode, alternate screen, stdin reader                                                                           |
| `src/client/keys.rs`         | Prefix key handling and the key bindings                                                                           |
| `tests/common/mod.rs`        | `TestServer`, `TestClient`, a PTY-driven client, linked test clusters and fakes for `ssh`, `tailscale` and the LAN |
| `tests/common/git.rs`        | Temporary repos with a local bare `origin` for the project tests                                                   |

Set `AMUX_LOG=debug` before the server starts to get more verbose logs.

## Tests

`cargo test` runs the unit tests and the integration tests in `tests/`. Each integration test starts its own server with a temporary `HOME`, `XDG_*` directories and socket, so it never touches your real server, config or state. Some tests drive the real `amux` binary inside a PTY. Cluster tests link several such servers on one machine through `exec:` addresses that run `amux bridge` with the other server's environment. Project tests work on temporary repos cloned from a local bare `origin`, and run git with their own identity and no user or system config.

Discovery is off in every test server unless the test turns it on, so no test touches the real tailnet or LAN. A fake `tailscale` (`AMUX_TAILSCALE`) serves `status` and `whois` from JSON files the test rewrites, a directory stands in for mDNS (`AMUX_LAN_DIR`), and a fake `ssh` (`AMUX_SSH`) maps host names to test servers. The one test that pairs over real mDNS, on a random service type set with `AMUX_MDNS_SERVICE`, is ignored by default because loopback has no multicast. `cargo test -- --ignored` runs it on a machine with a real network.

## Roadmap

amux is growing into a multiplexer that spans machines, following [docs/design.md](docs/design.md).

- [x] Config file, server name, ID and incarnation
- [x] Protocol version check on every connection
- [x] Transport-agnostic connection handling
- [x] Integration tests driving a real PTY
- [x] Cluster view: peer links over SSH, `amux servers`, and `amux ls` across machines
- [x] Remote sessions: `new --on`, `attach -t session@server`, reconnects
- [x] Projects and worktrees: `new -p -b`, `--clone`, `amux projects`
- [x] Windows within a session (`Ctrl-b c`, `n`, `p`)
- [x] Pane splits with a layout tree and a cell-level compositor
- [x] Status bar with cluster information, rename prompts and the `Ctrl-b s` cluster tree
- [x] A direct TCP transport with a keypair for each server (Noise), so links don't need sshd
- [x] Finding peers from `tailscale status`, LAN discovery over mDNS and `amux pair`
- [x] A trust store shared across the cluster, `amux servers forget` and `amux discover`

Phases 1 to 7 of the design are done. What remains is its phase 8, "Later":

- [ ] Session persistence: layouts and working directories that survive a server restart
- [ ] Predictive local echo for high-latency links, like mosh

Beyond the design:

- [x] A Lua config: `init.lua` and `servers.lua` in place of `config.toml`, and `amux config check`, `defaults` and `path`
- [ ] Prefix key and key bindings from the config in the client, which still uses the defaults
- [ ] Scrollback and copy mode
