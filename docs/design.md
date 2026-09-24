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

- There is one server per machine per user (and per `-L` name, as today). Its name defaults to the hostname and can be set in the config. A random ID is created on first start and stored in `$XDG_STATE_HOME/amux/server-id`. The name is for people to read, and the ID is how amux notices renames and duplicate names.
- A server is authoritative for its own sessions and project checkouts and for nothing else. There is no cluster-wide state, so amux needs no leader and no consensus.
- A server starts on demand. A local client starts it, as it does today, and so does a peer that connects over SSH, because `amux bridge` starts the server if it isn't running. A systemd user unit is optional and keeps a machine reachable right after boot.
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

- A peer link is a byte stream that carries the same length-prefixed postcard frames as the local socket. Any transport that provides a byte stream works.
- **v1 uses SSH.** The local server runs `ssh <host> amux bridge`. On the remote machine, `amux bridge` connects to the server's unix socket, starting the server if needed, and pipes stdin and stdout to it. amux opens no ports. SSH handles auth, encryption and host keys, and `~/.ssh/config` (identities, `ProxyJump`) applies as usual.
- **Tailscale** works through the same path. The address is the machine's tailnet name (`ssh://desktop`), served by a normal sshd over the tailnet or by Tailscale SSH.
- **Later:** a direct TCP transport with a keypair for each server (Noise). It would drop the dependency on sshd and the cost of one ssh process per link.

The trust model is that **cluster membership means full trust**. Any server in the cluster can spawn shells on any other, which gives the same access as SSH to that machine. A cluster is one user's machines, and every link runs as that user.

## Membership

- The config lists peers and their addresses. On startup a server dials each peer and keeps the link up, reconnecting with backoff when it drops.
- The handshake (`Hello`) carries the server ID, name, protocol version and the peers the server knows about, with their addresses. The server also dials the peers it learns this way. Adding a machine on one server is therefore enough, as long as the address resolves from every machine, which Tailscale names do. When addresses conflict, the local config wins.
- Each pair of servers shares exactly one link. If both sides dial at the same time, the link opened by the lower ID survives.
- A peer has one of three statuses:
  - `online`, shown with the link's latency
  - `offline`, shown with when it was last seen
  - `incompatible`, when its protocol version doesn't match
- When two servers claim the same name, the one that connects second is refused until it is renamed.

## State sync

- Each server publishes a snapshot of its own state:
  - the server's name, ID and version
  - its project checkouts
  - its sessions, each with project, branch, windows, attached clients and last activity
- When a link comes up, the two peers exchange full snapshots. After that they send deltas: `SessionCreated`, `SessionClosed`, `SessionChanged` and `ProjectsChanged`.
- Each server caches the latest snapshot of every peer. `amux ls` reads this cache and never waits on the network. An offline server's last known sessions are shown as stale.
- Only the owner changes its own state. Actions on another server's sessions, such as create, kill and rename, go to the owner as requests. The result comes back as a response and a state event.

## Protocol

| Layer           | Between                       | Carries                                                                           |
| --------------- | ----------------------------- | --------------------------------------------------------------------------------- |
| Client protocol | client ↔ local server         | Today's `ClientMessage` and `ServerMessage`, with targets that include the server |
| Peer protocol   | server ↔ server               | `Hello`, `Snapshot`, `Event`, `Request`/`Response` by id, `Channel` open/data/close |
| Channel         | inside a peer link            | Client protocol messages for one tunneled client                                  |

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

Each machine has its own `~/.config/amux/config.toml`:

```toml
name = "desktop"
projects_dir = "~/projects"

[servers.laptop]
address = "ssh://laptop"

[servers.home-server]
address = "ssh://notpc@home-server"
amux_path = "~/.cargo/bin/amux"

[projects.amux]
default_server = "desktop"
worktrees_dir = "~/projects/amux-worktrees"
```

`amux_path` covers machines where a non-interactive ssh shell doesn't have `amux` on its `PATH`.

## Failure

| Event                     | Result                                                                                                    |
| ------------------------- | --------------------------------------------------------------------------------------------------------- |
| Last pane in a session exits | The session closes, as it does today                                                                   |
| A server stops or its machine reboots | Its sessions die, and its peers mark it offline. Restoring layouts and cwds comes later.      |
| A link drops              | Sessions keep running on their hosts. Clients attached over the link reconnect and get a full redraw.     |
| The network partitions    | Each side keeps running its own sessions. Each server owns only its own state, so nothing needs merging when the partition heals. |

## Code layout

| Path                   | Responsibility                                         |
| ---------------------- | ------------------------------------------------------ |
| `src/config.rs`        | Config file, server name and ID                        |
| `src/target.rs`        | Target parsing and resolution                          |
| `src/project.rs`       | Git detection, project identity, worktrees             |
| `src/protocol/`        | Client protocol, peer protocol, framing, versioning    |
| `src/cluster/mod.rs`   | Membership, cached peer snapshots, request routing     |
| `src/cluster/link.rs`  | Peer link, reconnects, channel multiplexing            |
| `src/cluster/ssh.rs`   | SSH transport and `amux bridge`                        |

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
7. **Later**
   - The TCP and Noise transport
   - Session persistence
   - Predictive echo
   - Peer discovery from `tailscale status`
