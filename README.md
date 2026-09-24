# amux

A terminal multiplexer written in Rust.

## Usage

```sh
cargo run --                     # start the server if needed, then attach to the most recent session or create one here
cargo run -- new -s work         # create a session named "work"
cargo run -- attach -t work      # reattach (aliases: a, attach-session)
cargo run -- ls                  # list sessions
cargo run -- kill-server         # stop the server and all sessions
```

Inside a session, press `Ctrl-b d` to detach and `Ctrl-b Ctrl-b` to send a literal `Ctrl-b`.

`-L <name>` picks a named server socket in the runtime directory and `-S <path>` sets an explicit socket path. Both work like tmux's flags, so you can run an isolated dev server next to your usual one.

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

`name` defaults to the hostname and `projects_dir` to `~/projects`. Only `name` does anything yet. The `servers` and `projects` tables are parsed now so the cluster work in [docs/design.md](docs/design.md) can use them.

Each server also has a random ID, stored in `$XDG_STATE_HOME/amux/<socket name>/server-id`, and a fresh incarnation ID every time it starts. The log shows both.

### Upgrading

Every connection starts with a greeting that carries the protocol version. When the client and the running server speak different major versions, the client names both and asks you to run `amux kill-server`. `kill-server` also works on a server that is too old to answer the greeting.

## Architecture

amux uses a client/server model like tmux. The server owns the shells and the client is only a view.

```
 terminal ── client ── unix socket ── server ─┬─ session ── pane ── PTY ── $SHELL
  (raw mode)                                  └─ session ── pane ── PTY ── $SHELL
```

- The **server** is started on demand as a detached `amux server` process. It listens on `$XDG_RUNTIME_DIR/amux-$UID/<name>` and logs to the same path with `.log` appended.
- Each **pane** spawns the user's shell in a PTY. Output goes through a `vt100` parser, so the server always holds the full screen state. That state is how a client gets a redraw when it reattaches. The shell doesn't inherit `SSH_*` variables from the server, and a session in a directory that doesn't exist fails instead of quietly starting in `$HOME`.
- Each attached **client** gets frames as a diff against the last screen it was sent. Frames are pulled, not pushed: a pane update only marks the client dirty, and the server renders a frame when the connection has room for one. A slow client skips intermediate screens, and its keystrokes never wait behind output. Keystrokes go back as raw bytes, and resizes are sent when `SIGWINCH` arrives.
- The **protocol** uses length-prefixed `postcard` frames. A connection opens with a `Greeting` and a `Welcome` whose layout never changes, then carries `ClientMessage` and `ServerMessage`. Any change to those messages bumps the major version. The server handles a connection as a `Duplex`, a pair of message channels, so it doesn't care what transport sits underneath.

| Path                          | Responsibility                                      |
| ----------------------------- | --------------------------------------------------- |
| `src/main.rs`                 | Parses the command line and dispatches              |
| `src/lib.rs`                  | The library the binary and the tests share          |
| `src/cli.rs`                  | Command-line interface                              |
| `src/paths.rs`                | Runtime, config and state paths                     |
| `src/config.rs`               | Config file, server ID and incarnation              |
| `src/protocol/mod.rs`         | Framing and the `Duplex` message channels           |
| `src/protocol/greeting.rs`    | Greeting, version constants and the version check   |
| `src/protocol/client.rs`      | Client and server messages                          |
| `src/server/mod.rs`           | Accept loop, session registry, shutdown             |
| `src/server/connection.rs`    | Per-client request handling and the attach loop     |
| `src/server/session.rs`       | Session state                                       |
| `src/server/pane.rs`          | PTY, shell process, terminal emulation              |
| `src/client/mod.rs`           | Commands, server bootstrap, attach relay            |
| `src/client/terminal.rs`      | Raw mode, alternate screen, stdin reader            |
| `src/client/keys.rs`          | Prefix key handling                                 |
| `tests/common/mod.rs`         | `TestServer`, `TestClient` and a PTY-driven client  |

Set `AMUX_LOG=debug` before the server starts to get more verbose logs.

## Tests

`cargo test` runs the unit tests and the integration tests in `tests/`. Each integration test starts its own server with a temporary `HOME`, `XDG_*` directories and socket, so it never touches your real server, config or state. Some tests drive the real `amux` binary inside a PTY.

## Roadmap

amux is growing into a multiplexer that spans machines, following [docs/design.md](docs/design.md).

- [x] Config file, server name, ID and incarnation
- [x] Protocol version check on every connection
- [x] Transport-agnostic connection handling
- [x] Integration tests driving a real PTY
- [ ] Cluster view: peer links over SSH, `amux servers`, and `amux ls` across machines
- [ ] Remote sessions: `new --on`, `attach -t session@server`, reconnects
- [ ] Projects and worktrees: `new -p -b`, `--clone`, `amux projects`
- [ ] Windows within a session (`Ctrl-b c`, `n`, `p`)
- [ ] Pane splits with a layout tree and a cell-level compositor
- [ ] Status bar and the `Ctrl-b s` cluster tree
- [ ] Prefix key, shell and key bindings in the config file
- [ ] Scrollback and copy mode
