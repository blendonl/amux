# amux

A terminal multiplexer written in Rust.

## Usage

```sh
cargo run --                     # start the server if needed, then attach to the most recent session or create one here
cargo run -- new -s work         # create a session named "work"
cargo run -- attach -t work      # reattach (aliases: a, attach-session)
cargo run -- ls                  # list the sessions on every server in the cluster
cargo run -- servers             # status, latency and version of every server
cargo run -- servers add laptop ssh://laptop
cargo run -- servers remove laptop
cargo run -- kill-server         # stop the server and all sessions
```

Inside a session, press `Ctrl-b d` to detach and `Ctrl-b Ctrl-b` to send a literal `Ctrl-b`.

`-L <name>` picks a named server socket in the runtime directory and `-S <path>` sets an explicit socket path. Both work like tmux's flags, so you can run an isolated dev server next to your usual one. Each `-L` name is its own cluster: a `-L dev` server only links to other `-L dev` servers.

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

`name` defaults to the hostname and `projects_dir` to `~/projects`. Each entry under `servers` is a peer to link to. The `projects` table is parsed now so the project work in [docs/design.md](docs/design.md) can use it.

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

## Architecture

amux uses a client/server model like tmux. The server owns the shells and the client is only a view.

```
 terminal ── client ── unix socket ── server ─┬─ session ── pane ── PTY ── $SHELL
  (raw mode)                            ║     └─ session ── pane ── PTY ── $SHELL
                                        ║ peer link (ssh … amux bridge)
                                        ╚══════════ server on another machine
```

- The **server** is started on demand as a detached `amux server` process. It listens on `$XDG_RUNTIME_DIR/amux-$UID/<name>` and logs to the same path with `.log` appended.
- Each **pane** spawns the user's shell in a PTY. Output goes through a `vt100` parser, so the server always holds the full screen state. That state is how a client gets a redraw when it reattaches. The shell doesn't inherit `SSH_*` variables from the server, and a session in a directory that doesn't exist fails instead of quietly starting in `$HOME`.
- Each attached **client** gets frames as a diff against the last screen it was sent. Frames are pulled, not pushed: a pane update only marks the client dirty, and the server renders a frame when the connection has room for one. A slow client skips intermediate screens, and its keystrokes never wait behind output. Keystrokes go back as raw bytes, and resizes are sent when `SIGWINCH` arrives.
- The **protocol** uses length-prefixed `postcard` frames. A connection opens with a `Greeting` and a `Welcome` whose layout never changes, then carries `ClientMessage` and `ServerMessage`. Any change to those messages bumps the major version. The server handles a connection as a `Duplex`, a pair of message channels, so it doesn't care what transport sits underneath.
- A **peer link** carries the same frames. After the greeting both servers send a `Hello`, the lower ID decides whether the link is a duplicate, and then each side sends a snapshot of its sessions followed by events stamped with its incarnation and a sequence number, so stale or repeated updates are dropped. One writer drains a control lane (pongs, goodbyes) ahead of a bulk lane (snapshots and events), and the reader never waits on anything the other side controls, so a peer that stops reading cannot stall this one.

| Path                       | Responsibility                                                           |
| -------------------------- | ------------------------------------------------------------------------ |
| `src/main.rs`              | Parses the command line and dispatches                                   |
| `src/lib.rs`               | The library the binary and the tests share                               |
| `src/cli.rs`               | Command-line interface                                                   |
| `src/paths.rs`             | Runtime, config and state paths                                          |
| `src/config.rs`            | Config file, server ID and incarnation                                   |
| `src/protocol/mod.rs`      | Framing and the `Duplex` message channels                                |
| `src/protocol/greeting.rs` | Greeting, version constants and the version check                        |
| `src/protocol/client.rs`   | Client and server messages                                               |
| `src/protocol/peer.rs`     | Peer messages, snapshots and state events                                |
| `src/cluster/mod.rs`       | Membership, dial loops, peer cache                                       |
| `src/cluster/link.rs`      | Peer handshake, link lanes, pings                                        |
| `src/cluster/ssh.rs`       | Addresses, the SSH and exec transport, `amux bridge`                     |
| `src/server/mod.rs`        | Accept loop, session registry, state events, shutdown                    |
| `src/server/connection.rs` | Per-client request handling and the attach loop                          |
| `src/server/session.rs`    | Session state                                                            |
| `src/server/pane.rs`       | PTY, shell process, terminal emulation                                   |
| `src/client/mod.rs`        | Commands, server bootstrap, attach relay                                 |
| `src/client/listing.rs`    | `amux ls` and `amux servers` output                                      |
| `src/client/terminal.rs`   | Raw mode, alternate screen, stdin reader                                 |
| `src/client/keys.rs`       | Prefix key handling                                                      |
| `tests/common/mod.rs`      | `TestServer`, `TestClient`, a PTY-driven client and linked test clusters |

Set `AMUX_LOG=debug` before the server starts to get more verbose logs.

## Tests

`cargo test` runs the unit tests and the integration tests in `tests/`. Each integration test starts its own server with a temporary `HOME`, `XDG_*` directories and socket, so it never touches your real server, config or state. Some tests drive the real `amux` binary inside a PTY. Cluster tests link several such servers on one machine through `exec:` addresses that run `amux bridge` with the other server's environment.

## Roadmap

amux is growing into a multiplexer that spans machines, following [docs/design.md](docs/design.md).

- [x] Config file, server name, ID and incarnation
- [x] Protocol version check on every connection
- [x] Transport-agnostic connection handling
- [x] Integration tests driving a real PTY
- [x] Cluster view: peer links over SSH, `amux servers`, and `amux ls` across machines
- [ ] Remote sessions: `new --on`, `attach -t session@server`, reconnects
- [ ] Projects and worktrees: `new -p -b`, `--clone`, `amux projects`
- [ ] Windows within a session (`Ctrl-b c`, `n`, `p`)
- [ ] Pane splits with a layout tree and a cell-level compositor
- [ ] Status bar and the `Ctrl-b s` cluster tree
- [ ] Prefix key, shell and key bindings in the config file
- [ ] Scrollback and copy mode
