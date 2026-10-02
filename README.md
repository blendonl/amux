# amux

A terminal multiplexer written in Rust.

## Install

```sh
curl -fsSL https://github.com/blendonl/amux/releases/latest/download/install.sh | sh
```

The script downloads the latest release for Linux or macOS on x86_64 or arm64, checks it against its SHA-256 checksum and puts `amux` in `~/.local/bin`, one of the places amux looks for itself over SSH. Set `AMUX_VERSION=0.2.0` for another release and `AMUX_INSTALL_DIR` for another directory, on the `sh` side of the pipe. The Linux builds are static, so they run on any distribution. To build amux yourself instead, run `cargo install --git https://github.com/blendonl/amux`.

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
cargo run -- pair k7-4821-9930 --verbose # print each step of the pairing, to see where it stops
cargo run -- config check                # load init.lua and servers.lua and say which files were read
cargo run -- config defaults             # every default setting, as Lua for a starting init.lua
cargo run -- config reload               # make the running server load its config again
cargo run -- kill-server                 # stop the server and all sessions
cargo run -- update                      # install the latest release in place of this amux
cargo run -- update --check              # only say whether a newer release is out
```

Inside a session, press `Ctrl-b d` to detach and `Ctrl-b Ctrl-b` to send a literal `Ctrl-b`. The other keys after `Ctrl-b` manage windows and panes, as in tmux. `Ctrl-b s` starts a search: `p` finds a project, `w` a worktree, and `s` opens a tree of every session in the cluster. The bottom row is a status bar that shows the session, its server and its windows. The prefix, every binding, the status bar and the colours come from the [config](#config), and text pasted with bracketed paste reaches the pane whole, even when it contains the prefix.

`-L <name>` picks a named server socket in the runtime directory and `-S <path>` sets an explicit socket path. Both work like tmux's flags, so you can run an isolated dev server next to your usual one. Each `-L` name is its own cluster: a `-L dev` server only links to other `-L dev` servers.

### Windows and panes

A session holds numbered windows, and each window splits into panes, each running its own shell. The host lays the panes out and draws them into one screen with borders between them, and the border around the active pane is green.

| Keys                    | Action                                                 |
| ----------------------- | ------------------------------------------------------ |
| `Ctrl-b c`              | New window                                             |
| `Ctrl-b n`, `Ctrl-b p`  | Next and previous window                               |
| `Ctrl-b 0` … `Ctrl-b 9` | The window with that number                            |
| `Ctrl-b %`              | Split the active pane into left and right panes        |
| `Ctrl-b "`              | Split the active pane into top and bottom panes        |
| `Ctrl-b o`              | Next pane                                              |
| `Ctrl-b` arrow key      | The pane in that direction                             |
| `Ctrl-b x`              | Kill the active pane                                   |
| `Ctrl-b &`              | Kill the active window                                 |
| `Ctrl-b ,`              | Rename the active window                               |
| `Ctrl-b $`              | Rename the session                                     |
| `Ctrl-b [`              | Browse the active pane's history in copy mode          |
| `Ctrl-b ]`              | Paste the text last copied in copy mode                |
| `Ctrl-b s p`            | Find a project in your project directories and open it |
| `Ctrl-b s w`            | Find a worktree of this session's project and open it  |
| `Ctrl-b s s`            | Pick a session or window anywhere in the cluster       |
| `Ctrl-b r`              | Reload the config, here and on this server             |
| `Ctrl-b ?`              | Show every key that works after `Ctrl-b`               |
| `Ctrl-b d`              | Detach                                                 |
| `Ctrl-b Ctrl-b`         | Send a literal `Ctrl-b`                                |

- Windows are numbered from 0. A new window takes the lowest free number, and the others keep theirs when one closes. `amux ls` counts the windows of every session in the cluster.
- A split shares the pane's space equally with its siblings, and every shell is resized to its pane. A split that leaves no room for the new pane is refused, and the status bar says why.
- A pane whose shell exits leaves the layout and its neighbours take over its space. A window closes with its last pane, and the session ends with its last window.
- Every client attached to a session sees the same active window and pane. The window takes the size of the client that typed last, and a client with a smaller terminal sees the top-left part of it.
- While a window has more than one pane, the terminal reports mouse clicks to amux, and clicking a pane makes it the active one. Most terminals still select text when you hold Shift. A program that turns on mouse reporting itself, like `vim` with `mouse=a` or `htop`, gets the clicks inside its own pane in its own coordinates.
- These keys work the same on a session on another server: they travel over the peer link to the server that holds the session.

### Copy mode

`Ctrl-b [` puts the active pane in copy mode, where you scroll back through its history and copy text from it. The pane holds still while you look: amux shows a copy of it taken when you pressed the keys, the program keeps running underneath, and what it prints meanwhile shows when you leave. The top right corner shows `[line/total]`, the line the cursor is on out of all the lines of history and screen. The keys are vi's:

| Keys                             | Action                                                  |
| -------------------------------- | ------------------------------------------------------- |
| `h` `j` `k` `l`, arrow keys      | Move the cursor                                         |
| `w`, `b`, `e`                    | Next word, previous word, end of the word               |
| `W`, `B`, `E`                    | The same, for words that only spaces separate           |
| `0` or `Home`, `^`, `$` or `End` | Start of the line, its first character, end of the line |
| `g`, `G`                         | Top and bottom of the history                           |
| `H`, `M`, `L`                    | Top, middle and bottom line of the pane                 |
| `Ctrl-y`, `Ctrl-e`               | Scroll up and down a line                               |
| `Ctrl-u`, `Ctrl-d`               | Scroll up and down half a page                          |
| `PageUp`, `PageDown` or `Ctrl-f` | Scroll up and down a page                               |
| `r`                              | Take a new copy of the pane, keeping your place         |
| `v` or `Space`, `V`              | Start selecting characters, or whole lines              |
| `o`                              | Move the cursor to the other end of the selection       |
| `y` or `Enter`                   | Copy the selection and leave copy mode                  |
| `Escape`                         | Clear the selection, or leave when nothing is selected  |
| `q`, `Ctrl-c`                    | Leave copy mode                                         |

- A number before a key repeats it: `10k` moves up ten lines. Other keys do nothing, and pasted text is dropped.
- A selection runs from where you started it to the cursor, both ends included, and a wide character is taken whole. `y` copies it to the clipboard of the client that pressed it (see [Clipboard](docs/lua.md#clipboard)) and to the paste buffer. A line that wrapped at the edge of the pane comes out as one line, the blanks at the end of each line are dropped, and there is no newline after the last line. When the selection holds only blanks, or nothing is selected, `y` just leaves copy mode. `r` clears the selection.
- `Ctrl-b ]` types the paste buffer into the active pane as if you had pasted it: each newline becomes Enter, other control characters are dropped, and a program that turned on bracketed paste gets the text inside `ESC[200~ … ESC[201~`. The server keeps one paste buffer for all its sessions until it stops, so you can copy in one session and paste in another. A session on another server uses that server's buffer. With nothing copied yet, `Ctrl-b ]` says the paste buffer is empty.
- Copy mode belongs to the pane, so every client attached to the session sees it. It stays while you switch windows and panes, detach and attach again, or reload the config.
- The copy keys and the colours of the position and the selection come from the config of the server that holds the session, like the pane borders: `amux.keymap.set("copy", "K", "halfpage_up")` binds a key there, and `amux.opt.theme.copy_position` and `amux.opt.theme.copy_selection` colour the position and the selection. [docs/lua.md](docs/lua.md#copy-mode) lists the actions.
- `amux.action.copy_mode_page_up()` starts copy mode a page up, like tmux's `copy-mode -u`. It has no key, since `PageUp` after `Ctrl-b` turns the pages of the which-key popup. Bind it with `amux.keymap.set("root", "S-PageUp", amux.action.copy_mode_page_up())`.
- Images are hidden while you browse: the pane shows only its text, and the images come back when you leave copy mode.
- A program in the alternate screen, such as `vim`, `less` or `htop`, has no history there, so copy mode shows only its screen.

### Images

Programs in a pane can show images with the [kitty graphics protocol](https://sw.kovidgoyal.net/kitty/graphics-protocol/): `kitten icat`, yazi, chafa, timg, image.nvim, ratatui-image and the other programs that speak it, including the ones that think they run inside tmux and wrap their commands in tmux passthrough. amux keeps each image on the server and draws it into the frame as kitty's Unicode placeholder cells, so an image scrolls with its text, stays inside its pane and under the status bar and the popups, and comes back when you detach and attach again, on a remote session too.

Images show in terminals that draw those placeholders: kitty 0.28 or later, ghostty, and the amux Android app (see [Images on the phone](#images-on-the-phone)). In any other terminal you see the text under the image, and blank cells where a program printed placeholders itself.

When it attaches, the client asks the terminal whether it shows images: a kitty graphics query, `XTVERSION`, the cell size in pixels and a device attributes query, answered within 500 ms or not at all. `amux.opt.images.client` decides what to do with the answers. `"auto"`, the default, shows images when the terminal answers the kitty query and names itself kitty 0.28 or later, ghostty or the amux app. `"on"` shows them without asking, and `"off"` never asks and never shows them. A program that asks a pane whether images work is told yes only while the client that typed last shows them, and `amux.opt.pane.images = false` turns images off in new panes.

- Animations play in kitty. The frames a program adds, edits and composes, their gaps and its commands to start, stop and loop reach the terminal, and a client that attaches in the middle gets every frame and the animation's state. Ghostty and the amux app show only the first frame. A placement that gets a derived image, described next, isn't animated and shows the image as first sent. Frames count against `amux.opt.images.memory_mb` with their image, and a program that sends more frames than fit gets `ENOSPC`.
- A placement that crops its image, moves it inside its first cell, stretches it to a given number of columns and rows, or shows it at its own size without filling its cells is sent as a new image the server makes to look that way, in cells the size of those of the client that typed last. Past 4096×4096 pixels, or when it doesn't fit in `amux.opt.images.memory_mb`, the image is fitted whole into its cells instead. A client whose cells are another size sees the new image scaled to fit.
- An image that scrolls off the top of its pane is gone. Scrolling the terminal back doesn't bring it back.
- The server keeps `amux.opt.images.memory_mb` (320) MiB of images, as stored, and lets go of the least recently used image without a placement first. Each client's terminal gets at most `amux.opt.images.client_memory_mb` (256) MiB of images, as decoded: when that's full, the server deletes the images the client isn't showing, least recently used first, and an image that still doesn't fit shows the text under it.

Sixel images work too: `img2sixel`, `lsix`, gnuplot, `mpv --vo=sixel` and the other programs that print them. A pane says it has sixel in its device attributes, keeps 1024 colour registers and answers `XTSMGRAPHICS`, and amux sends each sixel image to the client like a kitty one. As in xterm and foot, the cursor ends on the image's last row at its first column, or right of the image with mode 8452, and text printed over a sixel cuts it. `amux.opt.pane.sixel = false` turns sixel off in new panes.

### Status bar and the cluster tree

The client keeps the bottom row of the terminal for a status bar, so a session gets one row less than the terminal has. With `amux.opt.status.enabled = false` the session gets every row, and errors and prompts cover the bottom row only while they show:

```
[notes@laptop] 0:sh  1:vim                            home-server offline  12 ms
```

- `[session@server]` is the attached session and the server that holds it.
- The windows follow, with the active one highlighted. When they don't fit, the list is cut around the active window and `…` marks the hidden ones.
- On the right are the servers that are offline and, for a session on another server, the latency of the link to it. On a narrow terminal the offline servers shrink to a count and then go away before any window does.
- When something you asked for fails, such as a split with no room, a rename to a name that is taken or a switch to a session that has just gone, the status bar shows the error for three seconds. The client stays attached.

`Ctrl-b s s` opens the cluster tree over the session: every server, then its projects, then their sessions and windows, starting on the session you are in.

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

While the tree, a picker or a prompt is open, keys and mouse clicks go to it and never reach the session. A lone `Escape` closes it after 50 ms, since it could also be the start of an arrow key.

### Search

`Ctrl-b s` is the search submap, and the key after it says what to find:

- `Ctrl-b s p` lists the git repositories in `amux.opt.search.project_dirs`, which are `~/projects` and `~/Projects` by default. A directory with a `.git` directory in it is a project, and amux doesn't look further inside it. With `amux.opt.search.project_depth = 2`, a repo one directory further down, such as `~/projects/work/api`, is found too. Hidden directories and worktrees, whose `.git` is a file, are skipped, a directory that doesn't exist is ignored, and a repo reached twice, through a symlink or two names for one directory, is listed once.
- `Ctrl-b s w` lists the worktrees of the attached session's project: its main checkout and every worktree on a branch, from this machine's checkout of the project.
- `Ctrl-b s s` opens the [cluster tree](#status-bar-and-the-cluster-tree).

The list opens over the session, with the best match at the top:

```
project> amx                                                              2/14
> amux  ~/projects/amux
  amux-old  ~/Projects/amux-old
```

- Typing filters the list. The letters of the query have to appear in order, a query in lowercase ignores case, and every word of a query with spaces has to match. Matches at the start of a word or in a run of letters rank first, and the matched letters are highlighted.
- `Up` and `Down`, or `Ctrl-p` and `Ctrl-n`, move. `Enter` opens the selected entry. `Backspace`, `Ctrl-w` and `Ctrl-u` delete a letter, a word and the whole query, and `Escape` or `Ctrl-c` closes the list.
- What you type while the list is loading filters it once it arrives.

Opening a project or a worktree does what `amux new` does in its directory: this client switches to the session of that checkout and branch, such as `amux/main` or `amux/feature-x`, and starts it when there is none. The first session in a repo registers the repo with this server, a repo on a detached `HEAD` gets a plain session in its directory, and the session runs on the project's `default_server` when it has one. When the session can't start, the status bar shows why and you stay in the session you were in.

The search runs on the machine you type on, so it lists that machine's repos and worktrees, also while you are attached to a session on another server.

### Which key

Like which-key in Neovim and Emacs, amux shows what the next key can do when you stop to think. Press `Ctrl-b` and wait half a second, and a popup at the bottom of the session lists every key of the prefix table with what it does:

```
─ C-b ──────────────────────────────────────────────────────────────────────────
 0     → window 0           c     → new window         %     → split left/right
 1     → window 1           d     → detach             &     → kill window
 …
 8     → window 8           "     → split top/bottom   C-b   → send prefix
 9     → window 9           $     → rename session
```

- The popup never changes what a key does. Pressing one runs it and closes the popup, and a key typed before the delay runs without the popup ever showing, so it stays out of the way once you know the keys.
- `Ctrl-b ?` shows the popup at once, whether or not it is turned on.
- A key that switches to another table (`switch_table`), a submap, shows as a group, `+resize`, and pressing it shows that table in place. The rule at the top names the keys typed so far, `C-b r`, and `Backspace` goes back one table.
- `Escape`, or any key the table doesn't bind, closes it. When the keys don't fit, the rule shows the page, `1/3`, and `PageDown` and `PageUp` turn it.
- The popup covers the bottom of the session while it shows, and the session is drawn again when it closes. Output from the pane keeps coming in around it.

Each key shows a short description of its action, and `amux.keymap.set` takes your own: `amux.keymap.set("prefix", "g", fn, { desc = "show the log" })`. A binding can be a sequence of keys, `amux.keymap.set("prefix", "g l", fn)`, which makes `g` a submap when it isn't one yet (see [Submaps](docs/lua.md#submaps)). `amux.opt.which_key` turns the popup off and changes its delay and its look, and the theme colours it. [docs/lua.md](docs/lua.md#which-key) has the details.

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

amux is configured in Lua. The server loads the config when it starts, the client loads it when it attaches (`amux`, `amux new` and `amux attach`), both load it again on a [reload](#reloading), and so do `amux servers add`, `amux servers remove` and `amux config`. `amux ls`, `kill`, `bridge`, `kill-server` and the other one-shot commands never read it. It runs the first of these that applies:

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

opt.search.project_dirs = { "~/projects", "~/work" }

opt.discovery.tailscale_tags = { "tag:server" }
opt.lan.port = 7448
```

Every option has a built-in default, so a config only sets what it changes. `amux.opt` is strict: an unknown option or a value of the wrong type is an error that names the file and line, such as `init.lua:3: unknown option amux.opt.bogus, expected one of …`. Values that would break the server, such as a zero interval or a blank `name`, are errors too. A bad config stops the server before it starts listening, and stops `amux`, `new` and `attach` before they take over the terminal, so a typo is never ignored.

The client and the server each read the config of the machine they run on. The client takes the prefix, the key bindings, the status bar, the tree and their colours from it, and the server everything the host does: panes, windows, sessions, pane borders, the cluster and hooks. Key bindings can call Lua functions, the status bar can show what a Lua function returns, and hooks run on server events:

```lua
amux.opt.prefix = "C-a"
amux.keymap.set("prefix", "a", amux.action.send_prefix())
amux.keymap.set("root", "M-h", amux.action.select_pane("left"))
amux.keymap.set("prefix", "g", function(ctx)
  amux.run(amux.action.new_window())
  amux.send_keys("git status\r")
end)

amux.opt.theme.status = { fg = "black", bg = "#8ec07c" }
amux.opt.status.interval_ms = 1000
amux.opt.status.right = function(ctx) return os.date(" %H:%M ") end

amux.opt.pane.term = "tmux-256color"
amux.opt.window.base_index = 1

amux.on("session_created", function(event) amux.log("new session " .. event.session) end)
```

[docs/lua.md](docs/lua.md) is the reference for `amux.keymap`, `amux.action`, the functions a binding can call, status functions, the theme slots and hooks.

| Command                | Does                                                                                        |
| ---------------------- | ------------------------------------------------------------------------------------------- |
| `amux config check`    | Loads the config as the server and as the client, then prints the files it read             |
| `amux config defaults` | Prints every default as assignments to `amux.opt`, which works as a starting `init.lua`     |
| `amux config path`     | Prints where your `init.lua` is or goes: the `--config` file, or the one in the config dir  |
| `amux config reload`   | Makes the running server load its config again, and prints what happened                    |
| `amux config keyboard` | Loads the config as the client and prints the Android app's landscape keyboard as JSON      |

`name` defaults to the hostname and `projects_dir` to `~/projects`. A leading `~` in `projects_dir`, `worktrees_dir` and `search.project_dirs` means your home directory. Each entry under `servers` is a peer to link to. Each entry under `projects` is keyed by project name: `default_server` is where `amux new` puts that project's sessions when you don't pass `--on`, and `worktrees_dir` is where its worktrees go instead of the default `<checkout>/../<project>-worktrees`.

```lua
amux.opt.pane.scrollback = 10000
```

`amux.opt.pane.scrollback` is the number of lines of history each pane keeps, which is how far back copy mode can go. The default is 2,000 and the most is 100,000. History costs about 36 bytes per cell, so a busy 200-column pane holds roughly 14 MB of it at the default and 72 MB at 10,000 lines. A reload applies a new value to the panes opened after it, and running panes keep the history they started with.

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

`amux.opt.clipboard` decides where the text you copy goes. With `osc52`, the default, the client that copied writes it to its terminal as an OSC 52 sequence, which puts it on the clipboard of the machine the terminal runs on, through SSH too. `command` runs a program on the client's machine with the text on standard input, for terminals that ignore OSC 52. On Android, the app's terminal takes OSC strings of up to 8,192 characters, so about 6 KB of copied text reaches the phone's clipboard. [docs/lua.md](docs/lua.md#clipboard) has the details.

```lua
amux.opt.clipboard.command = { "wl-copy" }
```

#### Reloading

A change to the config takes effect without a restart:

- `Ctrl-b r` reloads the client you type in, then asks the server on this machine to reload too. It asks over a connection of its own, so the key never reloads the config of another machine, even while you are attached to a session there. The status bar then shows `config reloaded`, or the error.
- `amux config reload` asks the running server to reload and prints `config reloaded` or the error. Without a running server it says so and exits with success.
- `SIGHUP` makes the server reload too, and it writes the result to its log (`<socket>.log`).

The server reloads the file it started with. When it started without one, or with `/etc/amux/init.lua`, it looks again, so an `init.lua` you create later is found.

A config that fails to load changes nothing. The running settings, bindings and hooks stay, and the error names the file and line, as at startup, for example `init.lua:12: unexpected symbol near '='`. A config that loads replaces them all at once, including every hook and every function binding.

What a reload changes:

- **The client**: the prefix, the key bindings, the status bar, the tree, the which-key popup, the search directories, the theme, and the notice and escape times, right away. An open prompt, tree or which-key popup closes.
- **Panes and windows**: the ones opened after the reload get the new `pane`, `window` and session naming settings. Panes that are already running keep the shell, `TERM`, environment and history length they started with. Pane borders are drawn again with the new `borders` and theme.
- **Projects**: `projects` and `projects_dir` apply to the next session.
- **Images**: `images.memory_mb` and `images.client_memory_mb` apply right away, and so does `images.client` in the client. `pane.images` and `pane.sixel` apply to the panes opened after the reload.
- **The cluster**: servers added to `amux.opt.servers` or `servers.lua` are linked, and removed ones are dropped. The status interval applies right away, and the other `cluster` timings to the next dial, backoff and link.
- **A restart** (`amux kill-server`) is still needed for `name`, `discovery` and `lan`, because the server's identity, its listeners and its discovery sources are set up once. A reload keeps their running values, applies the rest, and says so: `config reloaded; restart required for amux.opt.name`.

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

`amux update` replaces the `amux` binary with the latest release. It checks the download against its SHA-256 checksum and runs it once before it swaps the file, so a broken download never replaces a working amux. `amux update --check` only says whether a newer release is out, and `amux update 0.2.0` installs that release, older or newer. It downloads with `curl`, unpacks with `tar` and needs write access to the directory `amux` is in, so an amux in `/usr/local/bin` updates with `sudo amux update`.

The update leaves the running server and its sessions alone, on the old binary. When the new release speaks the same protocol major version, amux keeps talking to that server, and `amux kill-server` switches to the new one when it suits you (this ends its sessions). When the major version changed, `amux update` says the server has to stop before you use amux again. It also names the servers in the cluster that run another release, because each machine updates on its own: run `amux update` there too, or `amux servers` to see every server's version.

`AMUX_RELEASES_URL` points `amux update` and `install.sh` at a mirror instead of GitHub. The mirror serves the latest release's tag as JSON at `<url>/latest`, like `{"tag_name": "v0.2.0"}`, and the release files under `<url>/download/v0.2.0/`.

Every connection starts with a greeting that carries the protocol version. When the client and the running server speak different major versions, the client names both and asks you to run `amux kill-server`. `kill-server` also works on a server that is too old to answer the greeting.

This version speaks protocol 9, which added config reloads. Protocol 8 added the steps `amux pair --verbose` prints, and protocol 7 added the Noise keys, trust updates and discovery, so every machine in the cluster needs the new build. Servers on different major versions refuse to link, and `amux servers` lists the older one as `incompatible, runs amux … (protocol 8.0)`. A build older than protocol 7 doesn't listen on TCP, so Tailscale and LAN discovery can't reach it either. The cluster cache of an older build still loads.

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
  amux pair k7-4821-9930 --host 192.168.1.23:40123
waiting for the other machine...
paired with laptop (5be0c7a1f29d4e8b93a6d10c7e42f851), key fingerprint 3f9a:1c07:88d2:e4b1
```

The first part of the code, `k7`, names the pairing window, which the server advertises over mDNS while it is open. The eight digits are the secret. `amux pair k7-4821-9930` on the other machine finds the server that advertises `k7`, pairs with it, and prints the same warning and a `paired with` line naming the first machine.

- Where multicast doesn't get through, as on many guest and office networks, run the `--host` line instead. `amux pair` prints one for each of the first machine's LAN addresses, every IPv4 address or, without one, every IPv6 address, so pick the one on the network the two machines share. That also works on Android, where `ip addr` can't read the addresses. `--host <ip[:port]>` takes an IP address, not a host name. Without a port, amux uses the port that mDNS saw for that address, or this machine's own `amux.opt.lan.port` if it is set. The joining machine doesn't need LAN discovery for this.
- After pairing through `--host`, the joining machine saves the first machine as a server at `tcp://<ip>:<port>`, just like `amux servers add`, and prints `saved <name> as a server at …`. That is how it links again after the link drops when mDNS can't reach the first machine, for example from inside WSL2. The saved address keeps working only while the first machine keeps that port, so set `amux.opt.lan.port` there. If the name is already configured, amux says so and keeps the existing entry.
- `amux pair --verbose` prints each step, with the time since it started, on either machine. The joining machine shows the address it dials and the local address it dials from, the connection, the Noise handshake and each step of the code exchange. The waiting machine shows its port, its LAN addresses and every connection that arrives. When the joining machine times out and the waiting one never prints `accepted a tcp connection`, the connection never reached amux there.
- `connecting to … timed out` means nothing answered at all: something on the way drops the connection, most often a firewall on either machine, or the address belongs to another machine. By default the LAN listener takes a new free port each time it opens, so where a firewall is in the way, set `amux.opt.lan.port` to a fixed port on that machine and allow it, for example `sudo ufw allow 7448/tcp`. Keep the rule, since linked machines dial that port again later.
- A window is good for one pairing and five minutes. It also closes after three attempts or when you leave `amux pair`, and only one `amux pair` can wait on a server at a time. One pairing connection runs at a time, and an attempt counts as soon as its first pairing message arrives. A wrong code fails on the machine that tried it, and the waiting machine prints how many attempts are left.
- The code itself never crosses the network. The two machines open a Noise connection and run SPAKE2 inside it, bound to both machines' keys and to that connection's handshake, so the exchange only succeeds between the two machines that hold the code.
- Pairing merges two clusters. The two machines trust each other first hand, the pairing connection becomes their first link, and trust spreads from there (see [Trust](#trust)): every machine linked to either one learns the keys of the other cluster and links to those machines wherever it can reach them. That is what the warning at the top is about.
- Afterwards the two find each other over mDNS whenever they share a LAN and multicast reaches both, even when DHCP moves them. Their address is `lan://<server id>`, and amux looks up the current IP addresses and port each time it dials.
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
- When the link drops while you are attached, the session keeps running on its server, a box saying "reconnecting to laptop…" covers the screen and the status bar lists laptop as offline. Once the link is back you get a full redraw. `Ctrl-b d` still detaches in the meantime, and `Ctrl-b s s` can switch to another session.
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

### Android

The Android app is a terminal that runs the real amux binary on the phone, with zsh, git and ssh in its panes. It opens straight into `amux`, which attaches to the most recent session or creates one, and a foreground service keeps the server running while the app is in the background. The phone then joins the cluster like any other machine (see [Joining the cluster from the phone](#joining-the-cluster-from-the-phone)).

The app needs Android 10 (API 29) or later on arm64 or x86_64, and its zsh, git and ssh only work for the phone's primary user (see [Limits](#limits)). Its build, unit tests and lint run in a container, and its binary and userland run in a Termux container, but it hasn't been tried on a real phone yet.

#### Building and installing

The Android SDK and NDK, Rust with the Android targets, and Gradle all live in a Docker image, and the userland is built in Termux's own build container, so the build needs nothing but Docker:

```sh
./android/build.sh all
adb install -r android/app/build/outputs/apk/debug/app-debug.apk
```

The first run builds the image, which is about 4 GB and takes several minutes, and then the userland, which takes much longer (see below). The APK is debug-signed and about 78 MB. For arm64-v8a and x86_64 it carries amux as `lib/<abi>/libamux.so`, the userland's 60 programs as `lib/<abi>/libu_*.so`, and the rest of the userland as `assets/userland/<abi>.zip`, about 17 MB each. The programs ship as native libraries because the native library directory is the only place an app on Android 10 and later may run its own files from.

| Command                              | Does                                                                                                           |
| ------------------------------------ | -------------------------------------------------------------------------------------------------------------- |
| `./android/build.sh image`           | Builds the `amux-android-build` image when it is missing or `android/docker/Dockerfile` has changed            |
| `./android/build.sh binary`          | Cross-compiles amux with the NDK and writes the stripped binaries to `android/app/src/main/jniLibs/`           |
| `./android/build.sh userland`        | Builds the userland for aarch64 and x86_64 in `android/.cache/userland/`, unless it is up to date              |
| `./android/build.sh userland-check`  | Checks the libraries the userland's programs need, and runs the x86_64 userland in a Termux container          |
| `./android/build.sh userland-inputs` | Prints the hash of the userland's inputs, which `userland` compares with the last build's to skip it           |
| `./android/build.sh package`         | Splits each userland into `libu_*.so` files in `jniLibs/` and a zip in `android/app/src/main/assets/userland/` |
| `./android/build.sh smoke`           | Runs the x86_64 binary and userland in a Termux container, laid out as the app lays them out                   |
| `./android/build.sh apk`             | Builds the debug APK from those files, runs the JVM unit tests and lint, and prints what the APK holds         |
| `./android/build.sh release-apk`     | Builds the release APK the same way, signed with the release key, and prints its signing certificate           |
| `./android/build.sh all`             | `image`, `binary`, `userland`, `package`, `smoke` and `apk`, in that order                                     |

Every step but `userland`, `userland-check` and `userland-inputs` builds the image first when it is out of date. A step that needs the output of an earlier one stops when that output is missing and names the step to run, so `apk` without the zips or the `libu_*.so` files says to run `package` first. The cargo and Gradle caches, the termux-packages checkout and the userland live in `android/.cache`, which git ignores.

`release-apk` is the step the release workflow runs (see [Releasing](#releasing)). It needs `AMUX_RELEASE_KEYSTORE`, the path of the keystore, `AMUX_RELEASE_KEYSTORE_PASSWORD` and `AMUX_RELEASE_KEY_ALIAS`, and stops when one is missing. It writes `android/app/build/outputs/apk/release/app-release.apk`, which isn't debuggable.

`userland` builds the packages in `android/userland/packages.txt`, and everything they depend on, with [termux-packages](https://github.com/termux/termux-packages). `android/userland/termux-packages.txt` pins its commit and its `ghcr.io/termux/package-builder` image. The step clones that commit into `android/.cache/termux-packages`, applies the patches in `android/userland/overlay/`, and builds every package from source for aarch64 and x86_64, with the app's package name, `io.github.blendonl.amux`, in place of Termux's. Termux's own packages can't be used, since every path in them points into Termux's data directory. The build runs in a container named `amux-userland-builder`, which stays for the next run. The step then extracts the packages into `android/.cache/userland/<arch>/prefix/`, without headers, static libraries and other build files, and collects their sources (see [Licenses and sources](#licenses-and-sources)).

The first `userland` run takes about 45 minutes on a 12-thread machine, on top of pulling the builder image, and the builder's image and container take about 18 GB of disk. Later runs skip the step while `packages.txt`, `termux-packages.txt`, the overlay and the part of `build.sh` that assembles the prefix stay the same. A change to the package list, the commit or the overlay builds every package again.

`userland-check` checks that no ELF file in the userland needs Berkeley DB or Kerberos, and that every library one needs is in the userland or in Android. Then it copies the x86_64 userland to `/data/data/io.github.blendonl.amux/files/usr` in `termux/termux-docker:x86_64` and, as the `system` user, runs zsh, bash, git with a commit, `ssh -V`, `ssh-keygen`, curl, nano, less, grep, sed and two shebang scripts.

`package` runs `android/userland/package.py` on each userland. Every ELF program becomes `jniLibs/<abi>/libu_<path>.so`, and hardlinks and identical copies ship once. Everything else goes into `assets/userland/<abi>.zip`: the shared libraries, scripts, data and licenses, a `SYMLINKS.txt` that links each program's path to its `libu_*.so` through `$filesDir/applib`, and a `USERLAND_VERSION` hash of the contents. The same userland always gives byte-identical files.

`smoke` checks that amux and the userland run on Android's libc, bionic, with nothing but what the app gives them. It uses `termux/termux-docker:x86_64` with `/bin` linked to `/system/bin`, as on a phone, installs the x86_64 zip there as the app does, with `applib` pointing at the `libu_*.so` files, and sets only the app's variables (see [The phone's environment](#the-phones-environment)).

- First it runs amux without the userland, as the fallback does: `amux --version` and `amux config check`, then `amux server`, waiting up to 10 seconds for `amux ls` to answer, and `amux kill-server`.
- Then, with the userland, it runs zsh and two shebang scripts through termux-exec, one of them `#!/usr/bin/env sh`, then `git init` and a commit, `ssh -V` and `curl -V`. It starts `amux server` again and opens a pane with `amux new` to check that the pane runs zsh.

It fails unless every step succeeds and the server exits with status 0 both times, and it prints the server's log. Docker has no SELinux, so the smoke test can't show that Android lets a pane run the userland's programs through `applib`. It doesn't run the APK either, which needs a phone or an emulator.

#### Using the app

- On its first start, and after an update that brings a different userland, the service installs zsh, git and ssh before it starts the server, and the app shows "Installing zsh, git and ssh…" with a percentage. The service unpacks the zip for the phone's ABI into `$filesDir/usr-staging`, applies its `SYMLINKS.txt` and swaps the result in for `$filesDir/usr`. An install that is cut short is cleaned up and runs again on the next start.
- If the install fails, the app shows the error with two buttons. Continue attaches anyway, and panes run Android's `/system/bin/sh` until the next time amux starts, which tries the install again. Stop amux stops the server and closes the app.
- `Ctrl-b d`, or the end of the session, shows a Detached panel. Reattach attaches again, and starts the server first when it has stopped. Stop amux stops the server, which ends every session on the phone, and closes the app.
- The service's notification has two actions. Keep awake holds a partial wake lock, which keeps the CPU running while the screen is off, and then reads Allow sleep. Stop runs `amux kill-server` and stops the service. On Android 13 and later the app asks for permission to show notifications when it opens.
- `amux kill-server` in a pane stops the service too, because the server exits cleanly.
- A server that fails to start or exits with an error starts again after 1 second, then 2, 4 and 8 seconds. When five starts in a row each fail within 30 seconds, the service gives up and its notification says why.
- The server's log is `$PREFIX/tmp/amux-<uid>/default.log`, or `$cacheDir/amux-<uid>/default.log` when panes fall back to `/system/bin/sh`. Once it passes 1 MiB, the service empties it before the next start.
- The screen stays on while the terminal shows.
- Android backups and device transfers leave out the app's data, so the server's key and trust store stay on the phone.

#### Keys

How you type depends on how you hold the phone. In portrait the app uses the phone's own soft keyboard, and in landscape it draws a split keyboard of its own on both sides of the terminal.

In portrait, a row of keys sits above the soft keyboard:

| Key             | Sends                                                                                                                        |
| --------------- | ---------------------------------------------------------------------------------------------------------------------------- |
| `Esc`, `Tab`    | That key                                                                                                                     |
| `Ctrl`, `Alt`   | The next key with that modifier, from the row or the keyboard. The key shows green while it waits, and a second tap drops it |
| `←` `↓` `↑` `→` | The arrow keys                                                                                                               |
| `Prefix`        | `Ctrl-b`, whatever `amux.opt.prefix` is set to                                                                               |

In landscape, the soft keyboard stays hidden. The app's keyboard is split into two halves that each take the full height of the screen, with the terminal between them: left half, terminal, right half. Each half takes 21% of the screen's width, which [`amux.opt.android.keyboard`](#the-landscape-keyboard) can change. With a hardware keyboard attached, landscape works like portrait.

The keyboard is made for two thumbs, and no key is on it twice. Each half is five keys wide: a top row, three rows of QWERTY and a thumb row.

```
Esc   Tab   '     -                             Ctrl  Alt   Paste ⏎
q     w     e     r     t                 y     u     i     o     p
a     s     d     f     g                 h     j     k     l     ;
z     x     c     v     b                 n     m     ,     .     /
[  ⇧  ][ nav ][Prefix]                    [ Space ][ ⌫ ][sym][num]
```

A layer key held by one thumb changes the other half, where the other thumb is free, and leaves its own half as it is:

- `sym`, on the right thumb, turns the left half into every symbol the base layer lacks: `! @ # $ %` and `^ & * ( )` on the top two rows, then `[`, `]`, `=`, `\` and `` ` `` above `{`, `}`, `+`, `|` and `~`.
- `num`, on the right thumb, turns the left half into a phone keypad, `1` to `9` with `0` under `8`, plus `F11` and `F12`. Holding a digit sends `F1` to `F10`, which its corner shows.
- `nav`, on the left thumb, turns the right half into the arrows on `h` `j` `k` `l`, with `Home`, `PgDn`, `PgUp` and `End` under them, `Del` and `Ins`. The top row stays, so `Ctrl` then `←` still works.
- `Prefix`, on the left thumb, sends `Ctrl-b`, whatever `amux.opt.prefix` is set to, when tapped. Held, it turns the right half into amux's prefix keys: new, previous, next, rename and kill window, split left-right and top-bottom, next and kill pane, help, the project, worktree and cluster pickers, reload, detach, rename session, and `Hide`.

The keys:

- A tap on a letter types it. Holding it for a moment opens a popup above it with its capital and its `Ctrl` and `Alt` forms, such as `F`, `^F` and `M-f` for `f`. Slide to one and let go to type it, or let go without sliding for the first. `i` and `m` have no `Ctrl` form, since those are `Tab` and `⏎`.
- A key that can be held types when you let go of it, unless you press another key first: then it types at once, so fast typing with both thumbs keeps its order.
- `Ctrl`, `Alt`, `⇧`, `sym`, `num` and `nav` all work the same way. A tap applies to the next key only, and shows the key's label in green meanwhile. A second tap locks it on and fills the key green, and a third tap turns it off. Holding the key applies it to every key you press until you let go, so one thumb can hold `nav` while the other taps arrows. While `Ctrl` or `Alt` is on, the letters show what they will send, such as `^C`.
- With `⇧`, a letter types its capital, and `'`, `-`, `;`, `,`, `.` and `/` type `"`, `_`, `:`, `<`, `>` and `?`. Digits and the keys on `sym` stay as they are.
- `⌫`, `Del`, the arrows, `PgUp` and `PgDn` repeat while held.
- `Paste` pastes the clipboard, as bracketed paste when the pane asks for it.
- `Hide` hides the keyboard so the terminal takes the whole width, and a tap on the terminal brings it back.

In both:

- Pinch to zoom. The app remembers the text size.
- Long-press to select text on the screen, then pick Copy or Paste from the menu that opens.
- A tap on the terminal brings the keyboard back. In a window with several panes amux turns on mouse reporting (see [Windows and panes](#windows-and-panes)), so the tap also reaches amux as a click and makes the pane under it active.

#### The landscape keyboard

The landscape keyboard is `amux.opt.android.keyboard` in `init.lua`, next to the rest of the [config](#config). Only the app reads it. The app runs `amux config keyboard`, which loads `init.lua` as the client does and prints the keyboard as JSON, when it attaches, whenever `init.lua` is saved, when the app comes back to the front and when the phone turns. `amux config defaults` prints the built-in layout, a good start for your own.

A mistake in the keyboard is a config error like any other: `amux config check` names it on any machine, and it stops `amux` and the server. When the config doesn't load, the app shows the error, such as `init.lua: amux.opt.android.keyboard.width.left must be between 5 and 45 percent of the screen, not 60`, and keeps the keyboard it had, or the built-in one when it has none yet.

On the phone, change it in a pane with nano, and the keyboard changes as soon as you save:

```sh
nano "$(amux config path)"
```

To give each half 30% of the screen:

```lua
amux.opt.android.keyboard.width.left = 30
amux.opt.android.keyboard.width.right = 30
```

| Setting                                     | Is                                                                                                                                                                                    |
| ------------------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `width.left`, `width.right`                 | Each half's share of the screen's width, in percent, from 5 to 45. The terminal gets the rest                                                                                         |
| `hold_ms`                                   | How long a key is held before its `hold` happens, 300 unless set, from 50 to 2000                                                                                                     |
| `taps_ms`                                   | How long a key with `taps` waits for another tap, 250 unless set, from 50 to 2000                                                                                                     |
| `layers.<name>.left`, `layers.<name>.right` | One half of a layer, as a list of rows from top to bottom. A row is a list of keys from left to right. The rows share the half's height equally, and the keys share their row's width |

`layers` works like `amux.opt.servers`: setting one layer, one half or one row keeps everything else, as `amux.opt.android.keyboard.layers.nav.right[2] = { … }` does. A new name adds a layer, `amux.opt.android.keyboard.layers.sym = nil` removes one, and setting all of `layers` replaces every layer. The keyboard starts on the `base` layer, which every layout needs with both halves. Any other layer can have just `left` or just `right`: the half it leaves out keeps showing the `base` layer, and when two layers are on, each half shows the one turned on last that has that half. Rows and keys count from 1, as Lua does, so an error names a key as `layers.base.left[2][3]`.

A key is one of these:

| Key                                                                                                                                                                                                                                                | Does                                                                                                         |
| -------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | ------------------------------------------------------------------------------------------------------------ |
| One character, such as `"q"`, `"{"` or `"é"`                                                                                                                                                                                                       | Types it, and with `Shift` its US-keyboard shifted form or its capital                                       |
| A key named as [`amux.keymap`](docs/lua.md#keys) names it: `"Escape"`, `"Tab"`, `"Enter"`, `"Backspace"`, `"Delete"`, `"Insert"`, `"Home"`, `"End"`, `"PageUp"`, `"PageDown"`, `"Up"`, `"Down"`, `"Left"`, `"Right"`, `"F1"` to `"F12"`, `"Space"` | That key. tmux's spellings such as `"Esc"`, `"BSpace"` and `"PgUp"` work too, in any case                    |
| One of those, or a character, after `C-`, `M-` or `S-`, such as `"C-c"`, `"M-Left"` or `"C-M-x"`                                                                                                                                                   | That key with `Ctrl`, `Alt` or `Shift`. `"C-c"` shows as `^C`                                                |
| `"Ctrl"`, `"Alt"`, `"Shift"`                                                                                                                                                                                                                       | That modifier                                                                                                |
| `"layer:<name>"`                                                                                                                                                                                                                                   | That layer, shown with the layer's name                                                                      |
| `"Prefix"`                                                                                                                                                                                                                                         | Sends amux's prefix, `Ctrl-b` unless `amux.opt.prefix` changes it                                            |
| `"Paste"`, `"Hide"`                                                                                                                                                                                                                                | Pastes the clipboard, or hides the keyboard                                                                  |
| `""`                                                                                                                                                                                                                                               | Nothing: an empty gap                                                                                        |
| `false`                                                                                                                                                                                                                                            | The key at the same place in the `base` layer. The `base` layer can't use it. A whole row can be `false` too |
| A table                                                                                                                                                                                                                                            | A key with more settings                                                                                     |

A table has exactly one of `key`, `text`, `send` and `prefix`, and any of the others:

| Field     | Is                                                                                                                                                                                                                                                                                         |
| --------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------ |
| `key`     | A key as in the table above                                                                                                                                                                                                                                                                |
| `text`    | Text the key types, such as `"git status\r"`. `Ctrl` and `Alt` apply to each of its characters                                                                                                                                                                                             |
| `send`    | Characters the key sends to the terminal as they are, without modifiers, such as `"\27[A"`. It shows them as `^[[A`                                                                                                                                                                        |
| `prefix`  | Keys to send after amux's prefix, such as `"c"` for a new window or `"s p"` for the project picker. It shows them unless `label` is set                                                                                                                                                    |
| `label`   | What the key shows instead                                                                                                                                                                                                                                                                 |
| `shift`   | What the key types with `Shift`                                                                                                                                                                                                                                                            |
| `width`   | The key's share of its row, 1 unless set. `"Space"` is 2 wide in the built-in layout                                                                                                                                                                                                       |
| `repeats` | `true` to repeat the key while it is held. A key with a `hold` can't repeat                                                                                                                                                                                                                |
| `hold`    | What holding the key does. A key, such as `"F1"`, is sent once the key has been held for `hold_ms`, and shows in the key's corner. `"Ctrl"`, `"Alt"`, `"Shift"` or `"layer:<name>"` stays on while the key is held, and a quick tap still sends the key. A list of keys opens a popup to pick one from |
| `taps`    | What quick taps send instead: the first key for two taps, the next for three, and so on. The key waits `taps_ms` after each tap before it sends anything                                                                                                                                  |

`Ctrl`, `Alt`, `Shift` and layer keys can't have `hold` or `taps`, since they already stay on while held and lock on a second tap. The keys in a `hold` or `taps` list can't have their own `hold`, `taps`, `width` or `repeats`, and only a single `hold` can be a modifier or a layer.

This keeps the built-in keyboard, but gives `e` accents to pick, a double tap on `Esc` that sends `Ctrl-c`, and word jumps in the empty row of `nav`:

```lua
local keyboard = amux.opt.android.keyboard
keyboard.layers.base.left[2][3] = { key = "e", hold = { "E", "é", "è", "ë" } }
keyboard.layers.base.left[1][1] = { key = "Escape", taps = { "C-c" } }
keyboard.layers.nav.right[2] = {
  { key = "M-b", label = "◂word" },
  { key = "M-f", label = "word▸" },
  "",
  "",
  "",
}
```

To type Space with the left thumb instead, swap the thumb rows around:

```lua
local layers = amux.opt.android.keyboard.layers
layers.base.left[5] = {
  "Shift",
  "layer:nav",
  { key = "Space", width = 2 },
  { key = "Prefix", hold = "layer:amux" },
}
layers.base.right[5] = { "Backspace", "layer:sym", "layer:num" }
```

#### Images on the phone

The app's terminal draws the images amux sends it, so `kitten icat`, yazi, chafa and the other programs in [Images](#images) show pictures on the phone as they do on the desktop. The terminal answers the client's probe with a kitty graphics OK and the name `amux-android(1)`, so the default `amux.opt.images.client = "auto"` turns images on.

- The terminal takes the kitty graphics commands the amux client writes: PNG, RGB and RGBA images sent in the command itself, compressed with `o=z` or not, virtual placements, deletes and queries. It draws an image only through the Unicode placeholder cells amux paints, never at the cursor, and reads no files or shared memory. Programs in panes can still use all of those, because amux turns them into placeholders. It leaves out animation frames, so an animated image shows its first frame.
- The terminal keeps at most 48 MiB of images, as received, and lets go of the least recently used one past that. The view keeps at most 128 MiB of decoded bitmaps, least recently used out first.
- The view decodes an image on the UI thread the first time it draws it, scaled down to at most 4096 pixels a side, so a large image can hold up that one frame.
- Each image is fitted whole into the cells of its placement, keeping its shape and centred, as kitty does.

To try it, run this in a pane. It shows a 2×2 picture of red, green, blue and white pixels across 8 columns and 4 rows:

```sh
printf '\e_Ga=T,f=24,s=2,v=2,c=8,r=4;%s\e\\' "$(printf '\377\0\0\0\377\0\0\0\377\377\377\377' | base64)"
```

A small PNG on the phone works the same way with `f=100`: `printf '\e_Ga=T,f=100;%s\e\\' "$(base64 -w0 picture.png)"`.

The emulator's and the view's unit tests cover this, and `GraphicsFixturesTest` replays the exact bytes the amux client writes for an upload, as stored or derived by the server, its placement and its placeholder cells, which `cargo test` keeps in `android/terminal-emulator/src/test/resources/graphics/`. Images haven't been tried on a phone or an Android emulator yet.

#### zsh, git and ssh

The APK carries its own userland, built from Termux's package recipes. Panes run zsh, and these are on their `PATH`:

- zsh, bash, and dash as `sh`
- git, without gitk, git gui and git svn
- openssh's client tools: `ssh`, `scp`, `sftp`, `ssh-keygen`, `ssh-agent`, `ssh-add`, `ssh-keyscan` and `ssh-copy-id`, but no `sshd`
- curl, with the CA certificates it needs for HTTPS
- coreutils, grep, sed, findutils, diffutils, tar and gzip
- less, nano, and ncurses-utils, such as `clear`, `reset` and `tput`

`android/userland/packages.txt` lists these packages, plus termux-exec for scripts, and the libraries they need come with them.

- `$PREFIX` is `$filesDir/usr`, the userland's root, with the programs in `$PREFIX/bin` and their libraries in `$PREFIX/lib`, as in Termux.
- On its first start the app writes a `~/.zshrc`: `compinit` for completion, a `%n@<server name> %~ %# ` prompt, 10000 lines of history in `~/.zsh_history`, and Emacs keys with `bindkey -e`. It writes the file only when it is missing, so your changes stay.
- Scripts work through termux-exec, which every pane preloads. It runs a script as `<interpreter> <script>`, so the script is only read, and turns `/bin/…` and `/usr/bin/…` in a shebang into `$PREFIX/bin/…`, so `#!/bin/sh` and `#!/usr/bin/env bash` work.
- There is no `pkg`, `apt` or `dpkg`. Termux's packages wouldn't work anyway, since their paths point into Termux's data directory. To add a program, add its Termux package to `packages.txt` and build the APK again.
- Android doesn't let an app that targets API 29 or later run files from its own storage, and amux targets API 35. The userland's programs run because `$PREFIX/bin` links them through `$filesDir/applib` to the APK's `libu_*.so` native libraries, and the service points `applib` at the app's native library directory every time it starts. A program you download or copy onto the phone can't run that way. Run such an ELF file through Android's dynamic linker instead: `/system/bin/linker64 ./program`.

With git and ssh, the phone does what the other machines in the cluster do:

- `amux new` in a repo, and `-p`, `-b` and `--clone`, give project sessions and worktrees on the phone (see [Projects and worktrees](#projects-and-worktrees)).
- The phone's server links over `ssh://` addresses, such as `amux servers add laptop ssh://laptop` (see [Over SSH, configured by hand](#over-ssh-configured-by-hand)). The link runs ssh with `BatchMode=yes`, so the phone needs a key the other machine accepts: make one with `ssh-keygen`, which puts it in `~/.ssh`, and add it there with `ssh-copy-id laptop`.

#### The phone's environment

Everything amux keeps lives in the app's private storage. Below, `$filesDir` is the app's files directory, `/data/user/0/io.github.blendonl.amux/files` for the phone's main user, and `$cacheDir` is its cache directory. The app starts the server and the client with this environment, and every pane inherits it:

| Variable                                | Value                                                                                |
| --------------------------------------- | ------------------------------------------------------------------------------------ |
| `HOME`                                  | `$filesDir/home`                                                                     |
| `XDG_CONFIG_HOME`                       | `$filesDir/config`                                                                   |
| `XDG_STATE_HOME`                        | `$filesDir/state`                                                                    |
| `PREFIX`, `TERMUX__PREFIX`              | `$filesDir/usr`                                                                      |
| `TMPDIR`                                | `$PREFIX/tmp`, so the socket and the server's log are in `$PREFIX/tmp/amux-<uid>/`   |
| `SHELL`                                 | `$PREFIX/bin/zsh`                                                                    |
| `PATH`                                  | `$PREFIX/bin:/system/bin`                                                            |
| `LANG`                                  | `en_US.UTF-8`                                                                        |
| `LD_PRELOAD`                            | `$PREFIX/lib/libtermux-exec-direct-ld-preload.so`, which is termux-exec              |
| `TERMUX_EXEC__SYSTEM_LINKER_EXEC__MODE` | `disable`, so termux-exec runs programs directly, not through `/system/bin/linker64` |
| `TERMUX_APP__DATA_DIR`                  | The app's data directory, `/data/user/0/io.github.blendonl.amux` for the main user   |
| `TERMUX_APP__LEGACY_DATA_DIR`           | `/data/data/io.github.blendonl.amux`                                                 |
| `ANDROID__BUILD_VERSION_SDK`            | The phone's API level, such as `35`                                                  |

`ANDROID_ART_ROOT`, `ANDROID_ASSETS`, `ANDROID_DATA`, `ANDROID_I18N_ROOT`, `ANDROID_ROOT`, `ANDROID_RUNTIME_ROOT`, `ANDROID_STORAGE`, `ANDROID_TZDATA_ROOT`, `ASEC_MOUNTPOINT`, `BOOTCLASSPATH`, `DEX2OATBOOTCLASSPATH`, `EXTERNAL_STORAGE`, `LOOP_MOUNTPOINT` and `SYSTEMSERVERCLASSPATH` are passed through from Android when it sets them, and the client also gets `TERM=xterm-256color` and `COLORTERM=truecolor`. The userland's programs find their libraries in `$PREFIX/lib` through their RUNPATH, so there is no `LD_LIBRARY_PATH`.

When the userland isn't installed, because the install failed, panes fall back to Android's own shell, `/system/bin/sh` (mksh), with the tools in `/system/bin`. `TMPDIR` is then `$cacheDir`, so the socket and the log are in `$cacheDir/amux-<uid>/`, `SHELL` is `/system/bin/sh`, `PATH` is `$filesDir/bin:/system/bin` and `LANG` is `C.UTF-8`, and the `PREFIX`, `TERMUX_*`, `LD_PRELOAD` and `ANDROID__BUILD_VERSION_SDK` variables are left out.

- `$PREFIX/bin/amux` links to the packaged binary through `applib`, and in the fallback `$filesDir/bin/amux` does. The service renews the link every time it starts, since an update moves the binary. So `amux` works in any pane: `amux ls`, `amux pair`, `amux new` and the rest.
- The config is `$filesDir/config/amux/init.lua`, the path `amux config path` prints. On its first start the app writes it with one line that names the server after the phone, such as `amux.opt.name = "pixel-8-pro"`. The name is the device name from Settings, or the model when that is empty, or `android`, lowercased, with accents dropped and anything other than `a-z` and `0-9` turned into `-`. The app writes the file only when it is missing, so your changes stay.

Change the config in a pane with nano:

```sh
nano "$(amux config path)"
amux config check
```

`Ctrl-b r` then reloads it. A change to `name`, `discovery` or `lan` needs a restart instead (see [Reloading](#reloading)): run `amux kill-server` in a pane, which ends every session on the phone, then tap Reattach.

The debug APK is debuggable, unlike the release APK, so from a computer `adb shell run-as io.github.blendonl.amux` runs commands as the app, in its data directory:

```sh
adb shell run-as io.github.blendonl.amux cat files/config/amux/init.lua
adb shell "run-as io.github.blendonl.amux sh -c 'cat > files/config/amux/init.lua'" < init.lua
```

#### Joining the cluster from the phone

On a LAN, pair the phone with any machine in the cluster (see [On the LAN, with a pairing code](#on-the-lan-with-a-pairing-code)). Run `amux pair` on the desktop, then the command it prints in a pane on the phone:

```sh
amux pair k7-4821-9930
amux pair k7-4821-9930 --host 192.168.0.24:40123
```

The app holds a Wi-Fi multicast lock while its service runs, because Android drops multicast on Wi-Fi without one, and the phone needs multicast to find the desktop over mDNS. Where multicast doesn't get through anyway, add `--host <addr>:<port>` with the desktop's IP address and the port `amux pair` printed, as in the second line. The phone then saves the desktop as a server at that `tcp://` address.

On a tailnet, link the phone by hand. Tailnet discovery never dials phones (see [On the same tailnet](#on-the-same-tailnet)), and the phone's server has no `tailscale` CLI, so it can't vouch for the desktop through `tailscale whois` and doesn't listen on the tailnet. A `tcp://` link to a key that nothing vouches for is refused, so pair once, with `--host`, the desktop's tailnet IP address and the port `amux pair` printed, then point the phone at the desktop's tailnet listener:

```sh
amux pair k7-4821-9930 --host 100.101.7.12:40123
amux servers remove desktop
amux servers add desktop tcp://100.101.7.12:7447
```

The pairing saves the desktop under its name, `desktop` here, at the port of its LAN listener, which changes each time that listener opens unless `amux.opt.lan.port` is set there, so that address can go stale. The tailnet listener stays at port 7447, so the phone can dial it whenever the link drops. `servers add` refuses a name that is already configured, which is why the entry the pairing saved goes first.

Once paired, the phone is a member like any other. `amux ls` lists the sessions of every machine, `amux attach -t work@desktop` attaches to one, `amux new --on desktop` starts one there, and `Ctrl-b s s` switches between them. The rest of the cluster learns the phone's key (see [Trust](#trust)), and now that the phone trusts a key, its server listens on the LAN like any other (see [Listening ports](#listening-ports)).

#### Licenses and sources

The userland is other projects' software under their own licenses, GPL, LGPL, MIT, BSD and Apache among them. amux stays MIT: the APK carries the userland next to amux, and amux only runs its programs and never links them. Each package's license is in `$PREFIX/share/doc/<package>/` on the phone.

`./android/build.sh userland` collects the corresponding source in `android/.cache/userland/sources/`: the upstream source archives of the packages, `termux-packages.txt` with the termux-packages commit the recipes come from, `packages.txt`, the overlay patches and `build.sh`. Publish that directory next to any APK you distribute.

git, which is GPL-2.0-only, links OpenSSL 3. openssh is built without Kerberos, and `userland-check` fails when anything in the userland needs Kerberos or Berkeley DB.

#### Limits

- The userland only works for the phone's primary user. Its programs look for their libraries, and its scripts for their interpreters, under `/data/data/io.github.blendonl.amux/`, which is the primary user's data directory, so zsh, git and ssh don't work in a work profile or for a secondary user.
- `amux update` can't replace amux on the phone: when a newer release is out, it stops with `there are no amux releases for android; build amux from source`. amux comes with the APK, so update the app instead: install `amux-android.apk` from a newer release, which keeps the app's data. An APK you build yourself is signed with another key, so it only installs over another build of your own, with `adb install -r`.
- Android 12 and later limit the processes that apps start, which Android calls phantom processes, and kill them when there are more than 32 across the phone or when one uses a lot of CPU in the background. The server, the client and every pane's shell are such processes, so a pane's shell can stop without warning, and a server that is killed starts again without its sessions. To lift the limit, turn on "Disable child process restrictions" in the developer options on Android 14 and later, or run `adb shell settings put global settings_enable_monitor_phantom_procs false` from a computer on Android 12L and later. Android 12.0 takes `adb shell device_config set_sync_disabled_for_tests persistent` followed by `adb shell device_config put activity_manager max_phantom_processes 2147483647`.

#### Without the APK

In [Termux](https://termux.dev), amux builds as on any Linux machine:

```sh
pkg install rust git
git clone https://github.com/blendonl/amux && cd amux
cargo install --path .
```

`cargo install` puts `amux` in `~/.cargo/bin`. Panes of that build have Termux's packages on the `PATH`, so `git` works in them, and `ssh` does after `pkg install openssh`. The phantom-process limit above applies to Termux as well.

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
- **Images** come out of a pane's output before the `vt100` parser sees them: an APC scanner cuts out kitty graphics commands and unwraps tmux passthrough. The images go into one store for the whole server, under keys that are never reused, and each placement is anchored to the screen rows it covers, so it moves when they scroll and ends when the last of them leaves the screen. Each placement also gets a display key of its own, which is the id the client's terminal knows its image by, so the same image placed twice is sent twice. An animation's frames are kept with their image as kitty keeps them: a frame drawn over another frame stays as it was sent until a program edits it or composes onto it, and then the server draws it whole, blending as kitty does.
- **Sixel** images come through the DCS callbacks of the vendored `vt100` parser. The pane decodes each one, marks the cells it covers and moves the cursor as xterm does, and the pump stores the image, padded to whole cells, outside the parser lock. A sixel placement paints only the cells that are still marked, so text printed over it cuts it, and it ends when none are left or a newer sixel covers all of them.
- For a client that shows images, the compositor paints every placement as kitty Unicode placeholder cells: U+10EEEE with diacritics for the image row and column, and the display key as the foreground colour. They go through the differ, the borders and the client's chrome like any other cell. Placeholders a program prints itself are rewritten from its image and placement ids to the display key, and a client that can't show an image gets the text under it instead, or blanks for printed placeholders. A pane without images and without printed placeholders costs nothing extra.
- Each attached **client** has its own differ, which compares the new grid with the last one that client was sent, clipped to that client's terminal size, and sends only the changed cells. It moves the cursor and clears line by line, never the whole screen. Frames are pulled, not pushed: output in a visible pane or a layout change only marks the client dirty, and the server composes a frame when the connection has room for one. A slow client skips intermediate screens, and its keystrokes never wait behind output. Keystrokes go back as raw bytes, key bindings as commands, and resizes are sent when `SIGWINCH` arrives.
- Each attached client also has an **uploader**, which sends the images its frames use as `Image` messages: the stored bytes in chunks of up to 1 MiB, or 64 KiB when the client is on another server, and then a virtual placement of the placement's size. A frame may use an image before it arrives, since the terminal draws the cells again when it does. The uploader sends one image message after each frame and keeps going while no frame is due, so neither waits for the other. It keeps each terminal within `images.client_memory_mb`, counting each image's first frame, deletes an image from the terminal when its placement ends, and starts over on every attach. After an animated image's placement come its animation frames and state, and later changes follow as the difference between what the terminal has and what the store holds: new animation frames as sent, changed ones whole, and new gaps and controls. Deleting an animation frame sends the whole image again. The client turns those messages into kitty graphics commands, never mixing another command into an unfinished upload, and deletes everything it sent when it attaches again or exits.
- A terminal fits a virtual placement's whole image into its cells, so a placement with a source rectangle, a cell offset, a stretch or a size of its own would look wrong. For those the uploader sends a **derived image** instead: a blocking task decodes the stored image, crops, scales and pads it to the placement's cells at the session's cell size, and keeps it deflated in the store under the display key, counted against the store's quota and the first thing let go of when it fills. Until it's ready the frame uses the key as if its upload were still on its way. When the session's cell size changes, the uploader deletes each derived image from the terminal and sends it again for the new size. A sixel image is already padded to its cells, so it never needs one.
- The host decodes **mouse** reports from the client's input. A click focuses the pane under the pointer, and a report only reaches a pane whose program asked for that kind of event, re-encoded in that pane's coordinates and format. A lone `Escape` that could start a report is held for at most 25 ms, so it never gets stuck.
- The **protocol** uses length-prefixed `postcard` frames. A connection opens with a `Greeting` and a `Welcome` whose layout never changes, then carries `ClientMessage` and `ServerMessage`. Any change to those messages bumps the major version. The server handles a connection as a `Duplex`, a pair of message channels, so it doesn't care what transport sits underneath.
- A **peer link** carries the same frames. After the greeting both servers send a `Hello`, the lower ID decides whether the link is a duplicate, and then each side sends a snapshot of its sessions followed by events stamped with its incarnation and a sequence number, so stale or repeated updates are dropped. One writer drains a control lane (pongs, credit, goodbyes, trust updates) ahead of a bulk lane (snapshots, events and channel data), and the reader never waits on anything the other side controls, so a peer that stops reading cannot stall this one.
- Over **TCP**, a connection opens with a `TcpOpen` frame that says whether it is a link or a pairing, then runs a `Noise_XX_25519_ChaChaPoly_BLAKE2s` handshake whose prologue binds that frame. A pump seals and opens the Noise messages between the socket and an in-memory duplex stream, so the peer link runs its usual framing on top. A pairing runs SPAKE2 over the same kind of connection first and then carries on as a link.
- A **channel** tunnels one client connection through a peer link. Each server numbers the channels it opens, and the server hosting the session runs the channel through the same connection handler as a local client, except that it only ever looks up its own sessions. The host sends at most four frames ahead and waits for the opening server to pass each one on to its client, so frames stay pulled end to end. Each channel has its own capped queue on the receiving side: a channel that overflows is closed on its own and the link stays up. The opening server forwards client messages without reading them, apart from detaching, switching sessions, opening a new one and listing the cluster, and reattaches by the host's incarnation and session ID when a dropped link comes back.
- The client draws its own **chrome**: the status bar, the reconnect overlay, the prompts, the cluster tree and the search pickers. It sends the host its terminal size without the status row, in the first request, on every resize and on a reattach, so the host never draws there. The host sends the session's name, windows and active window when the client attaches and whenever they change. The server the client is connected to adds a cluster status (its own name, the session's server, the link latency and the offline servers) on attach, whenever the cluster changes and every two seconds. A channel never carries a cluster status, since the server at the client's end knows it best. The status bar is drawn again after each batch of output, saving and restoring the cursor around it. While the tree or a picker is open the client drops the session's output, and closing one of them or a prompt asks the host for a full redraw. A picker scans directories and runs `git` in blocking tasks of the client, and a pick sends the host a `NewSession`, which an attached client may send like a `Switch`: the server switches it to the new or reused session, or refuses and keeps it attached.
- A client's terminal size is clamped to at least 2 rows by 2 columns, on the client and on the server, because the terminal emulator can't handle anything smaller.
- **Git** runs through the `git` CLI in blocking tasks, never while the server holds its sessions lock. Registry changes are serialized and saved before they are published as a `ProjectsChanged` event, and creates for the same project and branch wait on a per-worktree lock, so concurrent `amux new`s share one session.

| Path                          | Responsibility                                                                                                     |
| ----------------------------- | ------------------------------------------------------------------------------------------------------------------ |
| `src/main.rs`                 | Parses the command line and dispatches                                                                             |
| `src/lib.rs`                  | The library the binary and the tests share                                                                         |
| `src/cli.rs`                  | Command-line interface                                                                                             |
| `src/paths.rs`                | Runtime, config and state paths, and the `init.lua` lookup order                                                   |
| `src/config/`                 | `amux config check`, `defaults`, `path` and `keyboard`, and the checked, atomic rewrites of `servers.lua`          |
| `src/identity.rs`             | Server ID, incarnation and hostname                                                                                |
| `src/settings/`               | Every setting with its built-in default, as one `Settings` tree                                                    |
| `src/lua/`                    | The Lua runtime: the strict `amux.opt`, the `amux` API, data files like `servers.lua` and the Lua writer           |
| `src/target.rs`               | `session@server` targets: parsing, validation and resolution                                                       |
| `src/project/id.rs`           | Project ids from origin URLs, local paths and root commits                                                         |
| `src/project/detect.rs`       | Finding the project, branch and main checkout of a directory                                                       |
| `src/project/registry.rs`     | The `projects.toml` registry of local checkouts                                                                    |
| `src/project/worktree.rs`     | Default branch, worktrees, fetch, clone and removal                                                                |
| `src/project/git.rs`          | Running `git` without prompts, with an optional timeout                                                            |
| `src/protocol/mod.rs`         | Framing and the `Duplex` message channels                                                                          |
| `src/protocol/greeting.rs`    | Greeting, version constants and the version check                                                                  |
| `src/protocol/client.rs`      | Client and server messages                                                                                         |
| `src/protocol/peer.rs`        | Peer messages, snapshots and state events                                                                          |
| `src/protocol/key.rs`         | Public keys, their hex form and fingerprints                                                                       |
| `src/cluster/mod.rs`          | Membership, dial loops, peer cache                                                                                 |
| `src/cluster/link.rs`         | Peer handshake, link lanes, pings                                                                                  |
| `src/cluster/channel.rs`      | Channels over a link: ids, credit, capped inbound queues                                                           |
| `src/cluster/transport.rs`    | Addresses, how to dial each one, and how a link was authenticated                                                  |
| `src/cluster/ssh.rs`          | The SSH and exec transport, the remote amux lookup, `amux bridge`                                                  |
| `src/cluster/noise.rs`        | Noise keys, the TCP opening, the Noise handshake and the pump                                                      |
| `src/cluster/listener.rs`     | The tailnet and LAN listeners and the limits before authentication                                                 |
| `src/cluster/trust.rs`        | The `trust.toml` store and the rules for trust updates and tombstones                                              |
| `src/cluster/cache.rs`        | The cluster cache and reading older versions of it                                                                 |
| `src/discovery/mod.rs`        | Discovery sources, their state and the `amux discover` report                                                      |
| `src/discovery/tailscale.rs`  | Reading `tailscale status`, the tailnet listener and `tailscale whois`                                             |
| `src/discovery/lan.rs`        | LAN candidates, the advertisement and `lan://` endpoints                                                           |
| `src/discovery/mdns.rs`       | Advertising and browsing over mDNS                                                                                 |
| `src/discovery/directory.rs`  | A directory that stands in for mDNS in the tests                                                                   |
| `src/pairing.rs`              | Pairing codes and windows, and SPAKE2 inside Noise                                                                 |
| `src/server/mod.rs`           | Accept loop, session registry, state events, shutdown                                                              |
| `src/server/connection.rs`    | Per-client requests, target routing and the attach loop                                                            |
| `src/server/status.rs`        | The cluster status sent to attached clients                                                                        |
| `src/server/forward.rs`       | Forwarding a client to a session on another server, reconnects                                                     |
| `src/server/projects.rs`      | Project registry, checkouts, clones, worktree sessions and removal                                                 |
| `src/server/session.rs`       | Session state, its windows and the commands that change them                                                       |
| `src/server/window.rs`        | A window's layout, panes, active pane and mouse routing                                                            |
| `src/server/layout.rs`        | The pane layout tree: splits, rectangles, borders and neighbours                                                   |
| `src/server/render/`          | The compositor, image placeholders, the per-client differ and escape sequences                                     |
| `src/server/graphics/`        | Kitty graphics commands, the image store, placements and the sixel decoder                                         |
| `src/server/upload.rs`        | Sending each client the images its frames use, within its memory budget                                            |
| `src/server/mouse.rs`         | Decoding and re-encoding mouse reports                                                                             |
| `src/server/pane.rs`          | PTY, shell process, terminal emulation                                                                             |
| `src/client/mod.rs`           | Commands, server bootstrap, attaching                                                                              |
| `src/client/relay.rs`         | The attached client: keys, panels, chrome and switching sessions                                                   |
| `src/client/chrome/`          | Status bar, prompt, search picker, reconnect overlay, key decoding and drawing helpers                             |
| `src/client/tree.rs`          | The `Ctrl-b s s` cluster tree                                                                                      |
| `src/client/search.rs`        | Finding the repos in the project directories and a checkout's worktrees, and opening a pick                        |
| `src/client/fuzzy.rs`         | Fuzzy matching and ranking for the pickers                                                                         |
| `src/client/listing.rs`       | `amux ls`, `amux projects`, `amux servers`, `amux discover` and `amux pair` output                                 |
| `src/client/projects.rs`      | Resolving `-p` against the projects the cluster knows                                                              |
| `src/client/terminal.rs`      | Raw mode, alternate screen, stdin reader                                                                           |
| `src/client/keys.rs`          | Prefix key handling and the key bindings                                                                           |
| `src/update.rs`               | `amux update`: finding, checking and installing a release                                                          |
| `tests/common/mod.rs`         | `TestServer`, `TestClient`, a PTY-driven client, linked test clusters and fakes for `ssh`, `tailscale` and the LAN |
| `tests/common/git.rs`         | Temporary repos with a local bare `origin` for the project tests                                                   |
| `tests/common/releases.rs`    | A `file://` release mirror for the `amux update` and `install.sh` tests                                            |
| `android/`                    | The Android app, a Gradle project: the userland installer, the service that runs `amux server`, and the terminal   |
| `android/build.sh`            | Builds the image, amux and the userland, packages the userland, smoke-tests both in Termux and builds the APK      |
| `android/docker/`             | The build image: JDK 17, the Android SDK and NDK, Rust with the Android targets, and `cargo-ndk`                   |
| `android/terminal-emulator/`  | Termux's terminal emulator v0.118.3 and its `libtermux.so`, vendored; `UPSTREAM.md` lists the files amux changed   |
| `android/terminal-view/`      | Termux's terminal view v0.118.3, the Android view that draws the emulator's screen, vendored the same way          |
| `android/userland/`           | The userland's `packages.txt`, and the termux-packages commit and builder image `termux-packages.txt` pins         |
| `android/userland/overlay/`   | Patches to termux-packages, such as the app's package name and openssh without Kerberos or sshd                    |
| `android/userland/package.py` | Splits a userland into `libu_*.so` programs and a zip with the rest, `SYMLINKS.txt` and `USERLAND_VERSION`         |

Set `AMUX_LOG=debug` before the server starts to get more verbose logs.

## Tests

`cargo test` runs the unit tests and the integration tests in `tests/`. Each integration test starts its own server with a temporary `HOME`, `XDG_*` directories and socket, so it never touches your real server, config or state. Some tests drive the real `amux` binary inside a PTY. Cluster tests link several such servers on one machine through `exec:` addresses that run `amux bridge` with the other server's environment. Project tests work on temporary repos cloned from a local bare `origin`, and run git with their own identity and no user or system config.

Discovery is off in every test server unless the test turns it on, so no test touches the real tailnet or LAN. A fake `tailscale` (`AMUX_TAILSCALE`) serves `status` and `whois` from JSON files the test rewrites, a directory stands in for mDNS (`AMUX_LAN_DIR`), and a fake `ssh` (`AMUX_SSH`) maps host names to test servers. The one test that pairs over real mDNS, on a random service type set with `AMUX_MDNS_SERVICE`, is ignored by default because loopback has no multicast. `cargo test -- --ignored` runs it on a machine with a real network.

The `amux update` and `install.sh` tests serve fake releases from a `file://` directory through `AMUX_RELEASES_URL`, and update a copy of the binary in a temporary directory.

Some unit tests write the fixtures the Android tests read: the keyboard defaults in `android/app/src/test/resources/keyboard/`, and the bytes the client writes for images in `android/terminal-emulator/src/test/resources/graphics/`. They fail when a fixture is out of date, and `AMUX_UPDATE_FIXTURES=1 cargo test` writes them again.

## Releasing

Pushing a `v*` tag builds and publishes a release with [the release workflow](.github/workflows/release.yml). `scripts/release 0.2.0` sets 0.2.0 in `Cargo.toml` and `Cargo.lock`, commits, and tags `v0.2.0`, and pushing the tag tests, builds and publishes it:

```sh
scripts/release 0.2.0
git push origin main v0.2.0
```

The workflow refuses a tag that doesn't match the version in `Cargo.toml` and runs the tests on Linux. It builds amux with the `dist` profile for `x86_64` and `aarch64` on Linux (static, with musl) and on macOS, and publishes `amux-<target>.tar.gz` for each with a `.sha256` next to it, plus `install.sh`. `scripts/release-notes` writes the release notes from the conventional commits since the previous tag, and `scripts/release-notes HEAD` previews them. A tag with a `-`, like `v0.3.0-rc.1`, becomes a pre-release, which `amux update` only installs when you name it. A push that changes the workflow builds every target without publishing, and so does running it by hand from the Actions tab.

The `android` job runs `android/build.sh` with `binary`, `userland`, `package`, `smoke` and `release-apk`, and with `userland-check` when it built the userland rather than restoring it. It publishes `amux-android.apk`, signed with the release key, and `amux-android-sources.tar.gz`, the userland's sources that [Licenses and sources](#licenses-and-sources) asks for next to the APK, each with a `.sha256`. The userland build is by far the slowest step, so the job caches `android/.cache/userland`, keyed on the hash `userland-inputs` prints. The first run after the userland's inputs change builds it and saves it, and later runs, tags included, restore it and skip the build.

The job fails straight away without the release key. Make it once and store it in the repository's secrets, since every APK has to be signed with the same key to install over the one before:

```sh
keytool -genkeypair -keystore ~/amux-release.keystore -storetype PKCS12 -alias amux \
  -keyalg RSA -keysize 4096 -validity 10000 -dname CN=amux
base64 -w0 ~/amux-release.keystore | gh secret set ANDROID_KEYSTORE
gh secret set ANDROID_KEYSTORE_PASSWORD
```

`keytool` asks for a new password, and `gh secret set ANDROID_KEYSTORE_PASSWORD` for the same one. Keep the keystore and its password somewhere safe outside the repository: without them, a new APK can't update an installed one, and the app has to be uninstalled, which deletes its data.

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
- [x] Status bar with cluster information, rename prompts and the cluster tree
- [x] A direct TCP transport with a keypair for each server (Noise), so links don't need sshd
- [x] Finding peers from `tailscale status`, LAN discovery over mDNS and `amux pair`
- [x] A trust store shared across the cluster, `amux servers forget` and `amux discover`

Phases 1 to 7 of the design are done. What remains is its phase 8, "Later":

- [ ] Session persistence: layouts and working directories that survive a server restart
- [ ] Predictive local echo for high-latency links, like mosh

Beyond the design:

- [x] A Lua config: `init.lua` and `servers.lua` in place of `config.toml`, and `amux config check`, `defaults` and `path`
- [x] Prefix key and key bindings from the config in the client
- [x] Reloading the config without a restart: `Ctrl-b r`, `amux config reload` and `SIGHUP`
- [x] A which-key popup that lists the keys after the prefix, with descriptions from the config
- [x] Releases for Linux and macOS, an install script and `amux update`
- [x] An Android app that runs amux on a phone and joins the cluster, with zsh, git and ssh in its panes
- [x] Key sequences and submaps, and `Ctrl-b s` to fuzzy-find projects and worktrees
- [x] Images from the kitty graphics protocol, shown in kitty, ghostty and the Android app
- [ ] Scrollback and copy mode
