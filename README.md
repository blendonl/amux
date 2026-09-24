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
cargo run -- servers add laptop ssh://laptop
cargo run -- servers remove laptop
cargo run -- kill-server                 # stop the server and all sessions
```

Inside a session, press `Ctrl-b d` to detach and `Ctrl-b Ctrl-b` to send a literal `Ctrl-b`. The other keys after `Ctrl-b` manage windows and panes, as in tmux.

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
| `Ctrl-b d`              | Detach                                           |
| `Ctrl-b Ctrl-b`         | Send a literal `Ctrl-b`                          |

- Windows are numbered from 0. A new window takes the lowest free number, and the others keep theirs when one closes. `amux ls` counts the windows of every session in the cluster.
- A split shares the pane's space equally with its siblings, and every shell is resized to its pane. A split that leaves no room for the new pane is ignored.
- A pane whose shell exits leaves the layout and its neighbours take over its space. A window closes with its last pane, and the session ends with its last window.
- Every client attached to a session sees the same active window and pane. The window takes the size of the client that typed last, and a client with a smaller terminal sees the top-left part of it.
- While a window has more than one pane, the terminal reports mouse clicks to amux, and clicking a pane makes it the active one. Most terminals still select text when you hold Shift. A program that turns on mouse reporting itself, like `vim` with `mouse=a` or `htop`, gets the clicks inside its own pane in its own coordinates.
- These keys work the same on a session on another server: they travel over the peer link to the server that holds the session.

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

The server reads `$XDG_CONFIG_HOME/amux/config.toml` (usually `~/.config/amux/config.toml`) when it starts. `--config <path>` or `AMUX_CONFIG` points it somewhere else. A missing file means defaults, and unknown keys are an error, so a typo stops the server instead of being ignored.

```toml
name = "desktop"
projects_dir = "~/projects"

[servers.laptop]
address = "ssh://laptop"
amux_path = "~/.cargo/bin/amux"
socket = "default"

[projects.amux]
default_server = "desktop"
worktrees_dir = "~/projects/amux-worktrees"
```

`name` defaults to the hostname and `projects_dir` to `~/projects`. Each entry under `servers` is a peer to link to. Each entry under `projects` is keyed by project name: `default_server` is where `amux new` puts that project's sessions when you don't pass `--on`, and `worktrees_dir` is where its worktrees go instead of the default `<checkout>/../<project>-worktrees`.

A server's `address` is either:

- `ssh://[user@]host[:port]`, which runs `ssh -T -o BatchMode=yes -o ServerAliveInterval=15 [-p port] [user@]host <amux_path> -L <socket> bridge`. `amux_path` defaults to `amux` on the remote `PATH` and is passed to the remote shell as written, so `~` works. `socket` defaults to this server's own socket name.
- `exec:<command>`, which runs the command directly, split like a shell would split it but without a shell. The command has to end in `amux … bridge`. The tests use it to link servers on one machine.

`amux servers add` and `amux servers remove` edit the config file in place, keeping its comments, and tell a running server about the change.

Each server also has a random ID, stored in `$XDG_STATE_HOME/amux/<socket name>/server-id`, and a fresh incarnation ID every time it starts. The log shows both.

### Upgrading

Every connection starts with a greeting that carries the protocol version. When the client and the running server speak different major versions, the client names both and asks you to run `amux kill-server`. `kill-server` also works on a server that is too old to answer the greeting.

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

- A server dials every address in its config, and every address its peers have linked over, retrying with a backoff from 1 s up to a minute while a peer is away. Adding one machine on one server is enough for the rest to find it, as long as its address works from every machine. The local config wins over what peers report, and a learned address that isn't seen for a week is forgotten.
- On the other machine, `amux bridge` connects stdin and stdout to the local server and starts that server if it isn't running. amux opens no ports: SSH handles auth and encryption, and `~/.ssh/config` applies as usual. `BatchMode=yes` means an unknown host key or a passphrase prompt fails the link, and the reason shows in the server log.
- When both sides dial at once, the link dialed by the lower server ID survives. A server that restarts replaces its old link at once. A second server that claims a name another online server already uses is refused until it is renamed.
- `kill-server` sticks. A stopping server says goodbye on every link, and its peers mark it stopped and only dial it with `amux bridge --no-start`, so they never start it again behind your back. It comes back when you start it.
- Peers, their addresses, the stopped flag and the last known sessions are kept in `$XDG_STATE_HOME/amux/<socket name>/cluster-cache`, so an offline server still shows its last known sessions after a restart. A cache that fails to decode is dropped.
- Links send a ping every 5 seconds and drop a peer that stays silent for three of them. `AMUX_PING_INTERVAL_MS` changes the interval, which the tests use to notice a lost link quickly.

A server that `amux bridge` starts over SSH inherits a non-interactive SSH environment and may be killed by logind when the SSH session ends. To keep a machine reachable, run its server from a systemd user unit (`ExecStart=%h/.cargo/bin/amux server`) and allow it to outlive your login with `loginctl enable-linger`.

### Remote sessions

A client only ever talks to its local server. Attaching to a session on another server opens a channel over the peer link to that server, which treats the channel like any other attached client. `amux new --on laptop`, `attach`, `rename` and `kill` all work the same way whichever server holds the session.

- A session created on another server starts in that server's `$HOME`, because paths differ between machines. The client's `LANG`, `LC_*` and `COLORTERM` are applied to the new shell wherever it runs, in place of the server's own.
- When the link drops while you are attached, the session keeps running on its server and your terminal shows "reconnecting to laptop…". Once the link is back you get a full redraw. `Ctrl-b d` still detaches in the meantime.
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
                                        ║ peer link (ssh … amux bridge)
                                        ╚══════════ server on another machine
```

- The **server** is started on demand as a detached `amux server` process. It listens on `$XDG_RUNTIME_DIR/amux-$UID/<name>` and logs to the same path with `.log` appended.
- Each **pane** spawns the user's shell in a PTY. Output goes through a `vt100` parser, so the server always holds the full screen state. That state is how a client gets a redraw when it reattaches. The shell doesn't inherit `SSH_*` variables from the server, and a session in a directory that doesn't exist fails instead of quietly starting in `$HOME`.
- A **session** holds an ordered list of windows, and each **window** holds a layout tree and its panes. The layout splits the window into rectangles with a one-cell border between siblings, and each pane's PTY is sized to its rectangle.
- The **compositor** paints the active window into a grid of cells: every pane's cells are read straight from its `vt100` screen while that pane's parser is locked, so nothing is copied per frame, and the borders are drawn with box-drawing characters. The cursor and input modes come from the active pane.
- Each attached **client** has its own differ, which compares the new grid with the last one that client was sent, clipped to that client's terminal size, and sends only the changed cells. It moves the cursor and clears line by line, never the whole screen. Frames are pulled, not pushed: output in a visible pane or a layout change only marks the client dirty, and the server composes a frame when the connection has room for one. A slow client skips intermediate screens, and its keystrokes never wait behind output. Keystrokes go back as raw bytes, key bindings as commands, and resizes are sent when `SIGWINCH` arrives.
- The host decodes **mouse** reports from the client's input. A click focuses the pane under the pointer, and a report only reaches a pane whose program asked for that kind of event, re-encoded in that pane's coordinates and format. A lone `Escape` that could start a report is held for at most 25 ms, so it never gets stuck.
- The **protocol** uses length-prefixed `postcard` frames. A connection opens with a `Greeting` and a `Welcome` whose layout never changes, then carries `ClientMessage` and `ServerMessage`. Any change to those messages bumps the major version. The server handles a connection as a `Duplex`, a pair of message channels, so it doesn't care what transport sits underneath.
- A **peer link** carries the same frames. After the greeting both servers send a `Hello`, the lower ID decides whether the link is a duplicate, and then each side sends a snapshot of its sessions followed by events stamped with its incarnation and a sequence number, so stale or repeated updates are dropped. One writer drains a control lane (pongs, credit, goodbyes) ahead of a bulk lane (snapshots, events and channel data), and the reader never waits on anything the other side controls, so a peer that stops reading cannot stall this one.
- A **channel** tunnels one client connection through a peer link. Each server numbers the channels it opens, and the server hosting the session runs the channel through the same connection handler as a local client, except that it only ever looks up its own sessions. The host sends at most four frames ahead and waits for the opening server to pass each one on to its client, so frames stay pulled end to end. Each channel has its own capped queue on the receiving side: a channel that overflows is closed on its own and the link stays up. The opening server forwards client messages without reading them, apart from detaching, switching sessions and listing the cluster, and reattaches by the host's incarnation and session ID when a dropped link comes back.
- A client's terminal size is clamped to at least 2 rows by 2 columns, on the client and on the server, because the terminal emulator can't handle anything smaller.
- **Git** runs through the `git` CLI in blocking tasks, never while the server holds its sessions lock. Registry changes are serialized and saved before they are published as a `ProjectsChanged` event, and creates for the same project and branch wait on a per-worktree lock, so concurrent `amux new`s share one session.

| Path                       | Responsibility                                                           |
| -------------------------- | ------------------------------------------------------------------------ |
| `src/main.rs`              | Parses the command line and dispatches                                   |
| `src/lib.rs`               | The library the binary and the tests share                               |
| `src/cli.rs`               | Command-line interface                                                   |
| `src/paths.rs`             | Runtime, config and state paths                                          |
| `src/config.rs`            | Config file, server ID and incarnation                                   |
| `src/target.rs`            | `session@server` targets: parsing, validation and resolution             |
| `src/project/id.rs`        | Project ids from origin URLs, local paths and root commits               |
| `src/project/detect.rs`    | Finding the project, branch and main checkout of a directory             |
| `src/project/registry.rs`  | The `projects.toml` registry of local checkouts                          |
| `src/project/worktree.rs`  | Default branch, worktrees, fetch, clone and removal                      |
| `src/project/git.rs`       | Running `git` without prompts, with an optional timeout                  |
| `src/protocol/mod.rs`      | Framing and the `Duplex` message channels                                |
| `src/protocol/greeting.rs` | Greeting, version constants and the version check                        |
| `src/protocol/client.rs`   | Client and server messages                                               |
| `src/protocol/peer.rs`     | Peer messages, snapshots and state events                                |
| `src/cluster/mod.rs`       | Membership, dial loops, peer cache                                       |
| `src/cluster/link.rs`      | Peer handshake, link lanes, pings                                        |
| `src/cluster/channel.rs`   | Channels over a link: ids, credit, capped inbound queues                 |
| `src/cluster/ssh.rs`       | Addresses, the SSH and exec transport, `amux bridge`                     |
| `src/server/mod.rs`        | Accept loop, session registry, state events, shutdown                    |
| `src/server/connection.rs` | Per-client requests, target routing and the attach loop                  |
| `src/server/forward.rs`    | Forwarding a client to a session on another server, reconnects           |
| `src/server/projects.rs`   | Project registry, checkouts, clones, worktree sessions and removal       |
| `src/server/session.rs`    | Session state, its windows and the commands that change them             |
| `src/server/window.rs`     | A window's layout, panes, active pane and mouse routing                  |
| `src/server/layout.rs`     | The pane layout tree: splits, rectangles, borders and neighbours         |
| `src/server/render/`       | The compositor, the per-client differ and escape sequences               |
| `src/server/mouse.rs`      | Decoding and re-encoding mouse reports                                   |
| `src/server/pane.rs`       | PTY, shell process, terminal emulation                                   |
| `src/client/mod.rs`        | Commands, server bootstrap, attach relay                                 |
| `src/client/listing.rs`    | `amux ls`, `amux projects` and `amux servers` output                     |
| `src/client/projects.rs`   | Resolving `-p` against the projects the cluster knows                    |
| `src/client/overlay.rs`    | The "reconnecting to …" overlay                                          |
| `src/client/terminal.rs`   | Raw mode, alternate screen, stdin reader                                 |
| `src/client/keys.rs`       | Prefix key handling and the window and pane bindings                     |
| `tests/common/mod.rs`      | `TestServer`, `TestClient`, a PTY-driven client and linked test clusters |
| `tests/common/git.rs`      | Temporary repos with a local bare `origin` for the project tests         |

Set `AMUX_LOG=debug` before the server starts to get more verbose logs.

## Tests

`cargo test` runs the unit tests and the integration tests in `tests/`. Each integration test starts its own server with a temporary `HOME`, `XDG_*` directories and socket, so it never touches your real server, config or state. Some tests drive the real `amux` binary inside a PTY. Cluster tests link several such servers on one machine through `exec:` addresses that run `amux bridge` with the other server's environment. Project tests work on temporary repos cloned from a local bare `origin`, and run git with their own identity and no user or system config.

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
- [ ] Status bar and the `Ctrl-b s` cluster tree
- [ ] Prefix key, shell and key bindings in the config file
- [ ] Scrollback and copy mode
