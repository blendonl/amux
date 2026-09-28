# amux design

amux is a terminal multiplexer that spans machines. Every machine runs an amux server. The servers link into one cluster, so from any machine you can:

- see every server and every session in the cluster
- create a session on any machine you choose
- attach to any session and control it, wherever it runs

Sessions are organized around projects (git repos). Each worktree of a project gets its own session, and different projects can run on different machines.

## Model

```
cluster
 └─ server            one per machine (and user), identified by name
     └─ session       runs on exactly one server, usually bound to a project worktree
         └─ window
             └─ pane  a PTY running a process

project               a git repo, known cluster-wide, checked out on zero or more servers
```

### Server

- There is one server per machine per user (and per `-L` name, as today). Its name defaults to the hostname and can be set in the config. A random ID is created on first start and stored in `$XDG_STATE_HOME/amux/<socket name>/server-id`, next to the server's Noise key in `noise-key`. The name is for people to read, and the ID is how amux notices renames and duplicate names.
- A server is authoritative for its own sessions and project checkouts and for nothing else. There is no cluster-wide state, so amux needs no leader and no consensus.
- A server starts on demand. A local client starts it, as it does today, and so does a peer that connects over SSH, because `amux bridge` starts the server if it isn't running. Tailscale and LAN discovery only reach a server that is already running. A systemd user unit is optional and keeps a machine reachable right after boot.
- A server keeps running with zero sessions because it still holds the peer links. Only `kill-server` stops it.

### Project

- A project is a git repository. Across the cluster it is identified by its normalized `origin` URL, such as `github.com/blendonl/amux`. A repo without a remote uses its root commit hash instead. The name defaults to the repo's directory name.
- Each server keeps a registry that maps each project to its local checkout path. A project gets registered by `amux project add [path]`, or automatically the first time a session is created inside the repo.
- Servers advertise which projects they have checked out, so every machine knows where each project can run.

### Session

- A session runs on exactly one server, its **host**. Its processes live on the host, and every other machine only views it.
- A session is usually bound to a project and a branch, and it runs in that branch's worktree on the host. Unbound sessions still work like plain tmux sessions.
- There is one session per worktree. The name defaults to `<project>/<branch>`, for example `amux/main` or `amux/feature-x`. Names are unique on each server. Across the cluster, a session's identity is `<session>@<server>`.
- One project can have sessions on several servers at the same time, for example `amux/main@desktop` and `amux/feature-x@laptop`.

### Window and pane

These work the same as in tmux. A session has an ordered list of windows. Each window has a layout tree of panes, and each pane is a PTY that runs a process. Windows and panes always live on the session's host.

## Topology

```
  laptop                              desktop
 ┌─────────────────────────┐        ┌────────────────────────────────────┐
 │ terminal ─ client       │        │                                    │
 │              │ unix     │  peer  │                                    │
 │            server ══════╪══link══╪══ server ─ amux/main ─ windows ... │
 └──────────────╥──────────┘  (ssh) └──────────────╥─────────────────────┘
                ║                                  ║
                ╚══════ peer link ══ server ═══════╝
                                    home-server
```

- A client only talks to its **local** server over the unix socket. It never connects to another machine itself.
- Every server keeps a **peer link** to every other server, which makes a full mesh. A link carries cluster state and tunnels attached clients.
- Attaching to a remote session opens a **channel** over the peer link. The host treats the channel as an ordinary attached client. The local server forwards frames and input without interpreting them.

The client keeps a single connection whatever the target. Because the server caches the state of the whole cluster, `amux ls` answers instantly. Inside an attached client, switching from a local session to a remote one is just a new target. Reconnect logic lives in one place, the peer link.

## Transport

A peer link is a byte stream that carries the same length-prefixed postcard frames as the local socket. Any transport that provides a byte stream works, and there are two.

- **SSH.** The local server runs `ssh <host> amux bridge`. On the remote machine, `amux bridge` connects to the server's unix socket, starting the server if needed, and pipes stdin and stdout to it. SSH handles auth, encryption and host keys, and `~/.ssh/config` (identities, `ProxyJump`) applies as usual. Without a configured `amux_path`, the remote command is one quoted `sh -c` script that runs `amux` from the `PATH`, or else the first executable of `~/.cargo/bin/amux`, `~/.local/bin/amux`, `/usr/local/bin/amux` and `/opt/homebrew/bin/amux`, and otherwise prints "amux is not installed on this machine" and exits 127. The bridge's last line on stderr becomes the link's error.
- **Noise over TCP.** `tcp://host:port` and `lan://<server id>` addresses connect over TCP. The dialer first sends a `TcpOpen` frame, a magic number and a kind (`Link` or `Pair`). Both sides then run `Noise_XX_25519_ChaChaPoly_BLAKE2s` with that frame as the prologue, so a link can't be passed off as a pairing or the other way round. XX authenticates both static keys. After the handshake a pump seals whatever is written into one end of a `tokio::io::duplex` into Noise messages of at most 65535 bytes, each with a two-byte length, and opens the messages that arrive into the other direction. `link::dial` and `link::accept` run over the duplex unchanged. Only a peer greeting is accepted over TCP. Each server keeps its static key in `<state>/noise-key` (mode 0600) and sends the public half in `Hello.public_key`.
- **Authentication.** A link records how it was authenticated: `Ssh`, or `Noise { key, vouched_by }`, where the trust store, the tailnet or a pairing vouches for the key. A Noise key that nothing vouches for, or that was forgotten, is refused right after the Noise handshake, before this server sends its `Hello`. When the peer's `Hello` arrives, its server ID must be the one the trust store binds to the key. A key the tailnet or a pairing vouched for becomes trusted first hand instead, as does the key in the `Hello` of an SSH or exec link. A key that another server ID already holds is never trusted, and a Noise link that presents one is refused.
- **Tailnet whois.** When a Noise key isn't trusted yet and the other end is on the tailnet (100.64.0.0/10, `fd7a:115c:a1e0::/48` or an address `tailscale status` lists), each side runs `tailscale whois --json` on the other's address: the dialer on the address it dialed, the acceptor on the connection's source. The dialer binds its own tailnet address as the source so that the acceptor sees it. The tailnet vouches for a node with a tag from `tailscale_tags` or, when this machine isn't tagged itself, a node of this machine's user. It never vouches for this machine's own node. Lookups are cached for five seconds, and a failed lookup vouches for nobody.
- **Pairing.** A `Pair` connection runs the same Noise handshake and then SPAKE2 (Ed25519 group), with the code's eight digits as the password. Each side's SPAKE2 identity is a side label, that side's static key and the Noise handshake hash, so the shared key is bound to both static keys and to this handshake. The joiner confirms first with an HMAC-SHA256 over the handshake hash and both SPAKE2 messages, the host answers with its own HMAC, ID and name, and the joiner sends its ID and name. Both then trust the other's key first hand, and the connection carries on as a peer link.
- **Listeners.** amux listens on TCP only for peers, and only in two cases. The tailnet listener binds this machine's tailnet IPv4 while Tailscale discovery runs and Tailscale reports one, at 7447 for the default socket, a port in 7448–7947 derived from any other socket name, or `tailscale_port`, and re-binds when the address changes. The LAN listener binds `0.0.0.0` at `amux.opt.lan.port` (a free port by default) while LAN discovery is on and the trust store holds a key or a pairing window is open, and writes its port to `<state>/lan-port`. Before authentication a connection has ten seconds, at most sixteen handshakes run at once and further connections are dropped, and one pairing connection runs at a time. With both discovery sources off, amux opens no ports.
- **Tailscale** also works through SSH: `ssh://desktop`, served by a normal sshd over the tailnet or by Tailscale SSH.

The trust model is that **cluster membership means full trust**. Any server in the cluster can spawn shells on any other, which gives the same access as SSH to that machine. A cluster is one user's machines, and every link runs as that user. A machine joins in one of three ways, and each one proves who it is: an SSH login, a tailnet that vouches for it as the same user's machine or an allowed tag, or a pairing code entered on both machines. Its key then spreads through the cluster, so joining through one member joins all of them, and `amux servers forget` takes a machine out everywhere. Nothing links without authentication.

## Membership

- A server keeps one target per address, each with its own dial loop that reconnects with backoff from 1 s up to a minute. A target is **configured** (from the config), **discovered** (`tcp://<tailnet ip>:<port>` from Tailscale, `lan://<server id>` from the LAN) or **gossiped** (an address a peer linked over). When one address comes from several places, configured beats discovered and discovered beats gossiped.
- The handshake (`Hello`) carries the server ID, name, protocol version, Noise public key and the peers the server knows about, with their addresses. The server also dials the peers it learns this way. Adding a machine on one server is therefore enough, as long as the address resolves from every machine, which Tailscale names do. Discovered addresses are never gossiped, since each machine discovers for itself, and neither are `tcp://` addresses on loopback or an unspecified address.
- Each pair of servers shares exactly one link. If both sides dial at the same time, the link opened by the lower ID survives.
- A peer has one of three statuses:
  - `online`, shown with the link's latency
  - `offline`, shown with when it was last seen
  - `incompatible`, when its protocol version doesn't match
- When two servers claim the same name, the one that connects second is refused until it is renamed.

### Discovered targets

- A discovery source hands `Cluster::discovered(via, candidates)` its whole candidate list on every poll or change. A new candidate becomes a target. A candidate that reappears or changes wakes the dial loops of its peer, which reset their backoff and dial at once.
- A candidate that drops out and was never reached is removed. One that was reached before stays, marked **absent**. Absence counts per peer: when every discovered target of a peer is absent, none of its gossiped targets is dialed either, so a machine that left the tailnet isn't chased over an `ssh://` address a peer passed on. Configured addresses are still dialed. Cached discovered targets load as absent until the first poll.
- A discovered target stays hidden from `amux servers`, `amux ls` and the status bar until it is **verified**: it linked once, or the other side refused with a valid `Hello` (such as `Duplicate` when both sides dial at once, or `NameTaken`). Hidden targets back off up to 10 minutes. A `SelfDial` refusal doesn't verify a target but marks it as this server, which is persisted, never dialed and never dropped.
- Discovered targets always dial with `--no-start` semantics, so discovery never starts a server on another machine.
- Every target keeps its last error: the dial error, the bridge's last stderr line or the refusal. `amux discover` shows it, so "why isn't this machine showing up?" has an answer.
- LAN candidates come from mDNS (`_amux._tcp.local.`, instance name = server ID, TXT `name`, `key`, `cluster`, `proto` and `pair` while a window is open) and carry the current endpoints, looked up again at every dial. Only servers of the same `-L` cluster count, and only a trusted key makes one dialable. The rest show as not paired or pairing open.

### Trust store

`trust.toml` in the state dir holds `trusted = [{ id, name, key, introduced_by, direct }]` and `forgotten = [{ id, key }]`. Trust travels in its own `PeerMessage::Trust { trusted, forgotten }`, on the control lane when a link comes up and whenever the store changes, and never enters the cluster cache.

- **One key per ID.** First-hand evidence (an SSH or exec link, a Noise link the tailnet vouched for, a pairing) sets `direct` and replaces an older key for that ID. A key another ID already holds is never trusted for a second ID.
- **First-hand introducer.** A gossiped entry is stored with `introduced_by` set to the member that saw it first hand: the sender for a direct entry, the entry's own introducer otherwise. Gossip never changes a key the server already knows, never adds this server, and never adds an entry whose key or introducer was forgotten, or a second-hand entry for a forgotten ID.
- **Tombstones per (id, key).** Forgetting records `{ id, key }`. A tombstone refuses that key everywhere, and refuses the ID until it is trusted again under a key that isn't forgotten. A gossiped tombstone is skipped when this server holds first-hand evidence of a different key for that ID.
- **Cascade.** Forgetting a member also drops the keys it introduced, unless they were seen first hand. Every member that takes the tombstone does the same and drops its link to the forgotten server.
- **Pairing again.** Pairing clears any tombstone for the other machine's ID and trusts its key first hand. The other members keep the old key's tombstone and take the new key from the pairing member, because a first-hand entry is refused only when its key is forgotten.
- **Known gap.** A member that trusted the old key second hand and missed the tombstone still knows a key for that ID, so it ignores the new key. Once the tombstone reaches it, it takes the new key only from a first-hand entry, so it waits until the member that paired with the machine sends another trust update.

### Forget

`amux servers forget <name|id>` works on any peer, configured, gossiped or discovered. It drops the link, the peer record and every target of that peer, removes configured entries from `servers.lua` (an entry that `init.lua` sets stays, and the reply says so), drops the key, and records and gossips the `{ id, key }` tombstone. `check()` then refuses the forgotten ID or key on every transport, discovery skips it, and the keys it introduced go too. The only way back is a new key: `amux pair --new-key` on that machine rotates `<state>/noise-key` atomically, and pairing it with any member clears the tombstone for its ID there.

## State sync

- Each server publishes a snapshot of its own state:
  - the server's name, ID and version
  - its project checkouts
  - its sessions, each with project, branch, windows, attached clients and last activity
- When a link comes up, the two peers exchange full snapshots. After that they send deltas: `SessionCreated`, `SessionClosed`, `SessionChanged` and `ProjectsChanged`.
- Each server caches the latest snapshot of every peer. `amux ls` reads this cache and never waits on the network. An offline server's last known sessions are shown as stale.
- Only the owner changes its own state. Actions on another server's sessions, such as create, kill and rename, go to the owner as requests. The result comes back as a response and a state event.

## Protocol

| Layer           | Between               | Carries                                                                                      |
| --------------- | --------------------- | -------------------------------------------------------------------------------------------- |
| Client protocol | client ↔ local server | Today's `ClientMessage` and `ServerMessage`, with targets that include the server            |
| Peer protocol   | server ↔ server       | `Hello`, `Snapshot`, `Event`, `Request`/`Response` by id, `Channel` open/data/close, `Trust` |
| Channel         | inside a peer link    | Client protocol messages for one tunneled client                                             |

- **Versioning.** Postcard isn't self-describing, so a version mismatch corrupts data silently instead of failing loudly. `Hello` therefore carries a protocol version, and a mismatched major version refuses the link. Different machines will run different amux builds, so this check matters.
- **One handler for every connection.** `connection::handle` takes a message stream rather than a `UnixStream`. A local socket connection and a tunneled channel then run the same code.
- **Frames are pulled, not pushed.** The host renders a new frame only when the channel has room. On a slow link, intermediate states are skipped rather than queued, which fits the existing `FrameDiffer`: each frame is a diff against the last frame actually sent.

## Attach and rendering

- **The host composes the session.** It lays out the window's panes and borders, renders the window area into one frame and diffs it for each client, as it does today.
- **The client draws its own chrome**: the status bar and overlays such as the session tree and prompts. It reserves those rows and sends the host the remaining size. That way the status bar can show cluster information (current server, link latency, offline peers) without the host knowing about it.
- **Remote input and output** take the path client → local server → channel → host.
- **When a link drops,** the client shows a "reconnecting to desktop…" overlay while the session keeps running on the host. On reconnect the host sends a full frame from the screen state it always holds.
- **When several clients are attached,** the latest active client sets the window size (tmux's `window-size latest`). Clients on different machines rarely have the same terminal size.
- **Later:** predictive local echo, like mosh, for high-latency links.

## Addressing

The target syntax is `[session][@server][:window[.pane]]`. It keeps tmux's `session:window.pane` and adds `@server`.

| Target                   | Means                                                              |
| ------------------------ | ------------------------------------------------------------------ |
| `amux/main`              | The session with that name, which must be unique in the cluster    |
| `amux/main@desktop`      | That session on `desktop`                                          |
| `@laptop`                | The most recent session on `laptop`                                |
| `amux/main@desktop:2.1`  | Window 2, pane 1                                                   |

- If a name matches sessions on several servers, the command fails and lists the matches.
- When amux builds a session name, `@` and `:` become `-`.

## Creating a session

The host is chosen in this order:

1. The `--on <server>` flag
2. The project's `default_server` from the config
3. The local server

When you run `amux new` inside a repo, the client turns the current directory into a **project and branch** and sends those, not a path. Paths differ between machines, and the host resolves the project to its own checkout. A session with no project on a remote host starts in `$HOME`.

If the host has no checkout of the project, the command fails with a hint. With `--clone`, the host clones the repo from the project's remote into its `projects_dir`.

## Worktrees

- The project's main checkout backs the session for the default branch.
- `amux new -p amux -b feature-x` works like this on the host:
  1. It uses the existing worktree for `feature-x`, if there is one.
  2. If the branch exists, it runs `git worktree add <dir> feature-x`.
  3. Otherwise it runs `git worktree add -b feature-x <dir>` from the default branch.
- `<dir>` comes from the project's `worktrees_dir` and defaults to `<checkout>/../<project>-worktrees/<branch>`.
- Killing a session never deletes its worktree. `amux kill --remove-worktree` deletes it and refuses when the worktree has uncommitted changes.

## CLI

```sh
amux                                        # attach to the most recent session, or create one here
amux new                                    # session here; project and branch come from git
amux new -p amux -b feature-x --on desktop  # worktree session on another machine
amux attach -t amux/main@desktop
amux ls                                     # every session on every server
amux ls --by project
amux kill -t amux/feature-x@laptop
amux servers                                # status, latency and version of each server
amux servers add laptop ssh://laptop
amux servers forget laptop                  # take a machine out of the cluster and refuse its key
amux discover                               # what Tailscale and the LAN found, and how linking goes
amux pair                                   # print a one-time code, then `amux pair <code>` on the other machine
amux projects                               # projects and where they are checked out
amux project add [path]
```

`amux ls` groups sessions by server:

```
desktop (this server)
  amux/main              3 windows   attached
  amux/feature-x         1 window
laptop                   12 ms
  notes/main             1 window
home-server              offline, last seen 3h ago
  infra/main             2 windows   stale
```

Inside a session, `Ctrl-b s` opens a tree of servers, projects, sessions and windows. Picking a session on another server works the same way as picking a local one.

## Config

Each machine has its own Lua config. amux runs the first of `--config` or `AMUX_CONFIG`, `~/.config/amux/init.lua` (under `$XDG_CONFIG_HOME`) and `/etc/amux/init.lua`, and uses the built-in defaults when there is none. The user file replaces the system file, and amux never writes `/etc`:

```lua
local opt = amux.opt

opt.name = "desktop"
opt.projects_dir = "~/projects"

opt.servers.laptop = { address = "ssh://laptop" }
opt.servers["home-server"] = {
  address = "ssh://notpc@home-server",
  amux_path = "~/.cargo/bin/amux",
}

opt.projects.amux = {
  default_server = "desktop",
  worktrees_dir = "~/projects/amux-worktrees",
}
```

Every option lives in one `Settings` tree with a built-in default in Rust, and `amux.opt` is a strict view of it that rejects unknown options and wrong types at the line that sets them. The loaded tree is then checked for values that would break the server, such as a zero interval. Either kind of error stops the server before it listens, and the client before it enters raw mode.

The client runs the same file when it attaches, in its own Lua VM on the main thread, and takes what it draws from it: the prefix, the keymap, the status bar with its Lua functions, the tree and their theme slots. The server runs it on a thread that owns its VM and takes the rest, including the hooks. [lua.md](lua.md) is the API reference.

`amux servers add` and `remove` write only `servers.lua`, a data file in the config directory that amux owns. It is merged into `amux.opt.servers` before `init.lua` runs, so `init.lua` can read and override its entries. Each rewrite is checked by loading the whole config with it, then renamed into place.

`amux_path` covers machines where amux is neither on the `PATH` of a non-interactive ssh shell nor in `~/.cargo/bin`, `~/.local/bin`, `/usr/local/bin` or `/opt/homebrew/bin`.

`amux.opt.discovery` turns Tailscale discovery (`tailscale`) and LAN discovery (`lan`) on and off, both on by default, and takes `tailscale_tags` and `tailscale_port`. `amux.opt.lan.port` fixes the LAN listener's port, which is otherwise a free one.

## Failure

| Event                     | Result                                                                                                    |
| ------------------------- | --------------------------------------------------------------------------------------------------------- |
| Last pane in a session exits | The session closes, as it does today                                                                   |
| A server stops or its machine reboots | Its sessions die, and its peers mark it offline. Restoring layouts and cwds comes later.      |
| A link drops              | Sessions keep running on their hosts. Clients attached over the link reconnect and get a full redraw.     |
| The network partitions    | Each side keeps running its own sessions. Each server owns only its own state, so nothing needs merging when the partition heals. |

## Code layout

| Path                      | Responsibility                                      |
| ------------------------- | --------------------------------------------------- |
| `src/config/`             | `amux config` and the rewrites of `servers.lua`     |
| `src/identity.rs`         | Server ID, incarnation and hostname                 |
| `src/settings/`           | Every setting and its built-in default              |
| `src/lua/`                | The Lua runtime, `amux.opt`, bindings and hooks     |
| `src/target.rs`           | Target parsing and resolution                       |
| `src/project.rs`          | Git detection, project identity, worktrees          |
| `src/protocol/`           | Client protocol, peer protocol, framing, versioning |
| `src/cluster/mod.rs`      | Membership, cached peer snapshots, request routing  |
| `src/cluster/link.rs`     | Peer link, reconnects, channel multiplexing         |
| `src/cluster/ssh.rs`      | SSH transport and `amux bridge`                     |
| `src/cluster/noise.rs`    | Noise keys, the TCP opening and the encrypted pump  |
| `src/cluster/listener.rs` | The tailnet and LAN listeners                       |
| `src/cluster/trust.rs`    | The trust store and its gossip rules                |
| `src/discovery/`          | Tailscale and LAN discovery                         |
| `src/pairing.rs`          | Pairing codes, windows and SPAKE2                   |

## Phases

Each phase ends with something that works.

1. **Foundations**
   - Config file and server identity
   - Transport-agnostic connection handling
   - The protocol version check
2. **Cluster view**
   - Peer links over SSH and `amux bridge`
   - Snapshots and events
   - `amux servers`, and `amux ls` across machines
3. **Remote sessions**
   - Channels
   - `@server` targets
   - `new --on`, and remote attach and kill
   - The reconnect overlay
4. **Projects and worktrees**
   - `amux projects` and `amux project add`
   - `new -p -b`, `--clone` and `default_server`
5. **Windows and panes**
   - Windows and the layout tree
   - The pane compositor on the host
6. **Chrome**
   - A status bar with cluster information
   - The `Ctrl-b s` cluster tree
7. **Discovery and pairing**
   - The TCP and Noise transport, and a trust store shared across the cluster
   - Linking on the same tailnet, vouched for by `tailscale whois`
   - LAN discovery over mDNS and `amux pair`
   - `amux discover` and `amux servers forget`
8. **Later**
   - Session persistence
   - Predictive echo
