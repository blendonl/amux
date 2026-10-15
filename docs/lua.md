# Lua API reference

amux runs `init.lua` in two places, and each machine uses its own copy (see [Config](../README.md#config) for where it is found):

- **The client** runs it when it attaches: `amux`, `amux new` and `amux attach`. It uses the prefix, the key bindings, the status bar, the theme, the tree and the search, which is everything the client draws. A config error stops the client with `init.lua:N` before it takes over the terminal.
- **The server** runs it when it starts. It uses everything the host does: panes, windows, sessions, borders, the cluster and the hooks.

Both run it again in a new Lua state on a reload: `reload_config()` (`Ctrl-b r`) reloads the client and the server on its machine, `amux config reload` or `SIGHUP` the server, and saving `init.lua`, `servers.lua` or a module in `lua/` each of them, unless `amux.opt.reload.watch` is `false`. Nothing carries over from the old state, so a module loaded with `require` is loaded again and every binding and hook is replaced. When the new config fails, the old state keeps running and the error shows with its `init.lua:N`. The [README](../README.md#reloading) lists what a reload changes and what still needs a restart.

`amux.process` is `"client"` or `"server"`, so one file can do different things in each. `amux config check` loads the file both ways. `amux ls`, `kill`, `bridge`, `kill-server` and the other one-shot commands never run it.

`amux config lsp` writes the types of this whole API for lua-language-server, so an editor completes and checks `init.lua` as you type (see [Editor support](../README.md#editor-support)).

| Everywhere             | Does                                                          |
| ---------------------- | ------------------------------------------------------------- |
| `amux.opt`             | Every setting, `amux config defaults` lists them              |
| `amux.keymap`          | Key bindings, used by the client                              |
| `amux.action`          | Action constructors for bindings                              |
| `amux.on(event, fn)`   | Registers a hook, which only the server runs                  |
| `amux.process`         | `"client"` or `"server"`                                      |
| `amux.hostname()`      | This machine's hostname                                       |
| `amux.log(...)`        | Writes its arguments, tab-separated, to the server log        |

## Keys

A key is written like tmux writes it: `C-b` (Ctrl), `M-h` (Alt), `S-Left` (Shift), and combinations such as `C-M-Up`. Single characters stand for themselves (`%`, `"`, `G`), and named keys are `Space`, `Enter`, `Tab`, `BackTab`, `Backspace`, `Escape`, `Up`, `Down`, `Left`, `Right`, `Home`, `End`, `Insert`, `Delete`, `PageUp`, `PageDown` and `F1` to `F12`. tmux's spellings `BSpace`, `Esc`, `IC`, `DC`, `PPage`, `NPage`, `PgUp`, `PgDn` and `BTab` work too.

`amux.opt.prefix` is the prefix key, `"C-b"` by default. `amux.opt.escape_time_ms` (50) is how long a lone `Escape` waits for the rest of a sequence. `amux.opt.which_key` sets the popup that lists the keys of a table when you stop after the prefix, see [Which key](#which-key).

## amux.keymap

| Function                              | Does                                                                   |
| ------------------------------------- | ---------------------------------------------------------------------- |
| `amux.keymap.set(table, keys, binding, opts)` | Binds `keys` in `table`, replacing what they were bound to. `keys` is one key or a [sequence](#submaps). `opts` is optional, and `opts.desc` describes the key in the which-key popup |
| `amux.keymap.del(table, keys)`         | Unbinds `keys`. It is an error when `keys` is not bound there          |
| `amux.keymap.get(table, keys)`         | The action or function bound to `keys`, or `nil`                       |
| `amux.keymap.clear(table)`             | Unbinds every key in `table`, and removes a custom table               |

The tables are:

- `root`: keys that act as soon as they are typed, without the prefix. Empty by default. Everything else reaches the pane byte for byte, and only a lone `Escape` is ever held back, when a root binding starts with it (`M-h` does).
- `prefix`: the key after the prefix. The defaults are the keys in the [README](../README.md#windows-and-panes). A key it doesn't bind is dropped, and the prefix itself, when it isn't bound here, sends a literal prefix.
- `prompt`: the rename and `amux.prompt` prompts. It binds prompt actions by name: `submit`, `cancel`, `delete_backward`, `delete_forward`, `delete_line`, `cursor_left`, `cursor_right`, `cursor_start` and `cursor_end`. The search prompt of [copy mode](#copy-mode) uses it too, from the config of the server that holds the session.
- `tree`: the `Ctrl-b s s` tree. It binds tree actions by name: `down`, `up`, `top`, `bottom`, `collapse`, `expand`, `pick` and `cancel`.
- `picker`: the `Ctrl-b s p` and `Ctrl-b s w` lists (see [Search](#search)). It binds picker actions by name: `down`, `up`, `pick`, `cancel`, `delete_backward`, `delete_word` and `delete_line`, and a key it doesn't bind types into the query.
- `copy`: [copy mode](#copy-mode), read by the server that holds the session. It binds copy actions by name.
- `search`: the submap behind `Ctrl-b s`, a custom table amux starts with. It binds `p` to `search_projects`, `w` to `search_worktrees` and `s` to `cluster_tree`.
- Any other name is a custom table, created by its first `set`. `switch_table(name)` reads the next key from it, then goes back to `root`. `Backspace`, when the table doesn't bind it, goes back to the table whose key switched here, or to `root` from the first one.

In the `prompt`, `tree`, `picker` and `copy` tables, a modified key that isn't bound acts like the plain key, so `C-Left` moves like `Left`. Text pasted with bracketed paste (`ESC[200~ … ESC[201~`) always goes to the pane whole, unless the pane is in [copy mode](#copy-mode): root and prefix bindings don't fire inside a paste, and a paste right after the prefix cancels the prefix.

```lua
amux.opt.prefix = "C-a"
amux.keymap.set("prefix", "a", amux.action.send_prefix())
amux.keymap.set("prefix", "|", amux.action.split_pane("left-right"))
amux.keymap.set("prefix", "-", "next_pane")
amux.keymap.set("prefix", "N", { select_window = 3 })
amux.keymap.del("prefix", "&")
amux.keymap.set("root", "M-h", amux.action.select_pane("left"))
amux.keymap.set("root", "M-r", amux.action.switch_table("resize"), { desc = "resize" })
amux.keymap.set("resize", "h", amux.action.select_pane("left"), { desc = "left pane" })
amux.keymap.set("prompt", "C-w", "delete_line")
amux.keymap.set("tree", "Space", "pick")
amux.keymap.set("picker", "C-k", "up")
```

### Submaps

A submap is a table that a key switches to, like `Ctrl-b s` switches to `search`: the key after it is read from the submap, and then keys go back to `root`. `switch_table(name)` binds one by hand, and a key sequence binds through them:

```lua
amux.keymap.set("prefix", "g", amux.action.switch_table("git"), { desc = "git" })
amux.keymap.set("prefix", "g s", function() amux.send_keys("git status\r") end, { desc = "status" })
amux.keymap.set("prefix", "g l L", function() amux.send_keys("git log\r") end)
amux.keymap.set("prefix", "s f", amux.action.search_projects())
```

- The keys of a sequence are separated by spaces, and `Space` names the space bar. Every key but the last has to be a submap. The last is bound in the submap they lead to, so `"g s"` binds `s` in `git`.
- `set` makes a key that isn't bound yet into a new submap named after its table and key, so `"g l L"` above creates `git l` and binds `l` in `git` to it. Bind the key to `switch_table` yourself first, as with `git`, for a name and a description of your own.
- A sequence through a key that is bound to anything else is an error, such as `d in the prefix table is bound to detach, not to a submap`. `get` returns `nil` for it and `del` fails.
- `root`, `prefix` and custom tables take sequences. The `prompt`, `tree`, `picker` and `copy` tables bind single keys.
- The which-key popup shows a submap as a group, and `Backspace` goes back to the table before it.

## amux.action

`root`, `prefix` and custom tables bind an action or a function. An action is written as a constructor call, `amux.action.split_pane("left-right")`, as a bare constructor for actions without an argument, `amux.action.new_window`, as its name, `"new_window"`, or as a table, `{ split_pane = "left-right" }`. An unknown action is an error that lists the valid ones.

| Action                      | Does                                                   |
| --------------------------- | ------------------------------------------------------ |
| `detach()`                  | Detaches the client                                    |
| `send_prefix()`             | Sends the prefix key to the pane                       |
| `new_window()`              | Opens a window                                         |
| `next_window()`             | Selects the next window                                |
| `previous_window()`         | Selects the previous window                            |
| `select_window(n)`          | Selects window `n`                                     |
| `split_pane(split)`         | Splits the active pane, `"left-right"` or `"top-bottom"` |
| `next_pane()`               | Selects the next pane                                  |
| `select_pane(direction)`    | Selects the pane `"left"`, `"right"`, `"up"` or `"down"` |
| `kill_pane()`               | Kills the active pane                                  |
| `kill_window()`             | Kills the active window                                |
| `rename_window()`           | Opens the rename window prompt                         |
| `rename_session()`          | Opens the rename session prompt                        |
| `cluster_tree()`            | Opens the cluster tree                                 |
| `search_projects()`         | Opens the list of git repositories in the project directories |
| `search_worktrees()`        | Opens the list of worktrees of the attached session's project |
| `switch_table(name)`        | Reads the next key from table `name`                   |
| `reload_config()`           | Reloads the config here and on this machine's server   |
| `which_key(name)`           | Shows the keys of table `name` at once, then reads the next key from it, `Ctrl-b ?` shows the prefix table |
| `copy_mode()`               | Puts the active pane in [copy mode](#copy-mode), `Ctrl-b [` |
| `copy_mode_page_up()`       | Puts the active pane in copy mode a page up, or turns a page up in copy mode. It has no key |
| `paste_buffer()`            | Types the text last copied in copy mode into the active pane, `Ctrl-b ]` |

## Functions as bindings

A function bound to a key runs in the client when the key is pressed. It gets `ctx`, a read-only snapshot of the client:

| Field          | Is                                                                   |
| -------------- | -------------------------------------------------------------------- |
| `session`      | The attached session's name                                          |
| `server`       | The server that holds it                                             |
| `local_server` | This client's own server                                             |
| `window_index`, `window_name`, `panes` | The active window, when the session has sent it |
| `windows`      | Every window: `{ index, name, panes, active }`                       |
| `latency_ms`   | The latency to a remote session's server, when it is known           |
| `offline`      | The names of the servers that are offline                            |
| `width`        | The terminal width, in status functions only                         |

Inside a binding these work, and they take effect in order once the function returns:

| Function                     | Does                                                                         |
| ---------------------------- | ---------------------------------------------------------------------------- |
| `amux.notify(message)`       | Shows `message` on the status row for `notice_ms` (3000)                     |
| `amux.run(action)`           | Runs an action, as if its key was pressed                                    |
| `amux.send_keys(keys)`       | Types `keys` into the active pane, escape sequences included                 |
| `amux.switch(target)`        | Switches this client to a target such as `"notes@laptop"` or `"work:1"`      |
| `amux.prompt { label, initial, on_submit }` | Opens a prompt. `on_submit(text, ctx)` runs on `Enter`, with the same API. Cancelling runs nothing |
| `amux.state()`               | The same snapshot as `ctx`                                                   |
| `amux.keymap.set/del/clear`  | Changes the bindings from the next key on, and frees a replaced function     |

A binding has one second to finish. An error, or running past the budget, shows the error on the status row and applies none of the binding's effects or keymap changes. `notify`, `run`, `send_keys`, `switch` and `prompt` are errors outside a binding, and only the client has them.

```lua
amux.keymap.set("prefix", "g", function(ctx)
  amux.run(amux.action.new_window())
  amux.send_keys("git status\r")
end)

amux.keymap.set("prefix", "f", function(ctx)
  amux.prompt {
    label = "switch to",
    initial = ctx.session,
    on_submit = function(text) amux.switch(text) end,
  }
end)
```

## Copy mode

The `copy` table holds the keys of [copy mode](../README.md#copy-mode). The server that holds the session reads it from its own config, as it does the pane border colours, and a reload changes it in panes that are already in copy mode. It binds single keys to these actions:

| Action                      | Does                                                 | Keys              |
| --------------------------- | ---------------------------------------------------- | ----------------- |
| `cursor_left`               | Moves the cursor left                                | `h`, `Left`       |
| `cursor_down`               | Moves the cursor down                                | `j`, `Down`       |
| `cursor_up`                 | Moves the cursor up                                  | `k`, `Up`         |
| `cursor_right`              | Moves the cursor right                               | `l`, `Right`      |
| `next_word`                 | Start of the next word                               | `w`               |
| `previous_word`             | Start of this or the previous word                   | `b`               |
| `next_word_end`             | End of this or the next word                         | `e`               |
| `next_space`                | `next_word` for words that only spaces separate      | `W`               |
| `previous_space`            | `previous_word` for words that only spaces separate  | `B`               |
| `next_space_end`            | `next_word_end` for words that only spaces separate  | `E`               |
| `start_of_line`             | First column                                         | `0`, `Home`       |
| `back_to_indentation`       | First character of the line                          | `^`               |
| `end_of_line`               | Last character of the line                           | `$`, `End`        |
| `history_top`               | First line of the history                            | `g`               |
| `history_bottom`            | Last line of the screen                              | `G`               |
| `top_line`                  | Top line of the pane                                 | `H`               |
| `middle_line`               | Middle line of the pane                              | `M`               |
| `bottom_line`               | Bottom line of the pane                              | `L`               |
| `scroll_up`                 | Scrolls up a line                                    | `C-y`             |
| `scroll_down`               | Scrolls down a line                                  | `C-e`             |
| `halfpage_up`               | Scrolls up half a page                               | `C-u`             |
| `halfpage_down`             | Scrolls down half a page                             | `C-d`             |
| `page_up`                   | Scrolls up a page                                    | `PageUp`          |
| `page_down`                 | Scrolls down a page                                  | `PageDown`, `C-f` |
| `refresh_from_pane`         | Takes a new copy of the pane, as far from the bottom | `r`               |
| `begin_selection`           | Starts selecting characters at the cursor            | `v`, `Space`      |
| `select_line`               | Starts selecting whole lines at the cursor           | `V`               |
| `other_end`                 | Moves the cursor to the other end of the selection   | `o`               |
| `clear_selection`           | Clears the selection                                 |                   |
| `copy_selection_and_cancel` | Copies the selection and leaves copy mode            | `y`, `Enter`      |
| `clear_selection_or_cancel` | Clears the selection, or leaves when there is none   | `Escape`          |
| `cancel`                    | Leaves copy mode                                     | `q`, `C-c`        |
| `search_forward`            | Opens the prompt to search down the history          | `/`               |
| `search_backward`           | Opens the prompt to search up the history            | `?`               |
| `search_again`              | Goes to the next match in the search's direction     | `n`               |
| `search_reverse`            | Goes to the next match in the other direction        | `N`               |

The digits `1` to `9` start a count that repeats the next action, and `0` adds to a count once one has started. They aren't in the table. A key the table doesn't bind does nothing, and pasted text goes into the search prompt while it is open and is dropped otherwise.

`begin_selection` and `select_line` start a new selection at the cursor, even while one shows, and `refresh_from_pane` clears it. The selection is drawn in the `copy_selection` [theme](#the-theme) slot. `copy_selection_and_cancel` sends the text to the clipboard of the client that pressed the key, as set in [Clipboard](#clipboard), and keeps it in the server's paste buffer, which `paste_buffer()` types into the active pane.

`search_forward` and `search_backward` open a prompt on the bottom row of the pane, edited with the `prompt` table of the server's config, not the client's. `submit` searches and `cancel` closes only the prompt. A count before them goes to that match, and a count before `search_again` or `search_reverse` repeats it. The search is literal and smart-case: it ignores case unless the text holds a capital letter. It looks at each line on its own and wraps around the ends of the history. The prompt and the `search hit BOTTOM` and `pattern not found` messages are drawn in the `copy_prompt` theme slot.

```lua
amux.keymap.set("copy", "K", "halfpage_up")
amux.keymap.set("copy", "J", "halfpage_down")
amux.keymap.del("copy", "C-c")
amux.keymap.set("copy", "Escape", "cancel")
amux.keymap.set("root", "S-PageUp", amux.action.copy_mode_page_up())
amux.keymap.set("root", "M-p", amux.action.paste_buffer())
amux.opt.theme.copy_position = { fg = "black", bg = "cyan" }
amux.opt.theme.copy_selection = { fg = "black", bg = "yellow" }
amux.opt.theme.copy_prompt = { fg = "white", bg = "blue" }
amux.keymap.set("prompt", "C-w", "delete_line")
```

## Which key

After the prefix, or a key bound to `switch_table`, the client waits for the next key. When none comes within `delay_ms`, a popup at the bottom of the session lists every key of the table and what it does, the way which-key does in Neovim and Emacs. The next key works as it would without the popup, and closes it.

| Option         | Default | Is                                                                                  |
| -------------- | ------- | ----------------------------------------------------------------------------------- |
| `enabled`      | `true`  | Whether the popup opens on its own. `which_key(name)` shows it even when `false`    |
| `delay_ms`     | `500`   | How long the client waits for the next key before the popup opens, `0` opens it at once |
| `separator`    | `"→"`   | Between a key and its description                                                   |
| `group_marker` | `"+"`   | Before the description of a key that switches to another table                      |

All of them live under `amux.opt.which_key`.

- A key shows the description given with `amux.keymap.set(table, key, binding, { desc = … })`. Without one it shows a short name for its action, such as `new window` or `split left/right`, and `lua function` for a function. Binding the key again without `desc` drops its description.
- A key bound to `switch_table(name)` is a group. It shows as `+name`, or `+` and its `desc`, and pressing it shows that table in the same popup. The rule at the top names the keys typed so far, such as `C-b r`, or the table's name when an action switched to it.
- In the `prefix` table, the prefix key itself shows as `send prefix` unless you bind it.
- `Backspace` goes back one table, `Escape` or a key the table doesn't bind closes the popup, and `PageDown` and `PageUp` turn the pages when the keys don't fit. Each of these does what the table binds it to instead, when it binds it.

The keys are sorted like this: letters and digits, symbols, named keys, then keys with modifiers, and they fill the columns top to bottom.

```lua
amux.opt.which_key.delay_ms = 300
amux.opt.theme.which_key_key = { fg = "yellow", bold = true }
amux.keymap.set("prefix", "R", amux.action.switch_table("resize"), { desc = "resize" })
amux.keymap.set("resize", "h", amux.action.select_pane("left"), { desc = "go left" })
amux.keymap.set("resize", "?", amux.action.which_key("resize"))
```

## Search

`search_projects()` (`Ctrl-b s p`) lists the git repositories it finds in the project directories, and `search_worktrees()` (`Ctrl-b s w`) the worktrees of the attached session's project. The client runs both on its own machine, and picking an entry opens its session the way `amux new` would in that directory. The [README](../README.md#search) describes the lists.

| Option          | Default                      | Is                                                                         |
| --------------- | ---------------------------- | -------------------------------------------------------------------------- |
| `project_dirs`  | `{ "~/projects", "~/Projects" }` | The directories to look for repositories in. A leading `~` is your home directory, and a directory that doesn't exist is skipped |
| `project_depth` | `1`                          | How many levels below each directory to look. `1` finds `~/projects/amux`, `2` also `~/projects/work/api` |

Both live under `amux.opt.search`, and a directory with a `.git` directory in it is a repository, which the search doesn't look inside.

```lua
amux.opt.search.project_dirs = { "~/projects", "~/work", "/srv/git" }
amux.opt.search.project_depth = 2
```

## Images

Programs in a pane show images with the kitty graphics protocol or sixel, and a client sees them when its terminal can show kitty's Unicode placeholders. The [README](../README.md#images) describes which programs and terminals work.

| Option                    | Default  | Is                                                                                       |
| ------------------------- | -------- | ---------------------------------------------------------------------------------------- |
| `images.client`           | `"auto"` | Whether this client shows images: `"auto"` asks the terminal, `"on"` always, `"off"` never and doesn't ask |
| `images.memory_mb`        | `320`    | How many MiB of images the server keeps, counted as stored. When it's full, the least recently used image without a placement goes first |
| `images.client_memory_mb` | `256`    | How many MiB of images the server sends to each client's terminal, counted decoded. When it's full, images the client isn't showing are deleted, least recently used first |
| `pane.images`             | `true`   | Whether new panes accept images. With `false`, a pane ignores kitty graphics commands and never says images work |
| `pane.sixel`              | `true`   | Whether new panes also accept sixel images and say so in their device attributes. It needs `pane.images` too |

They live under `amux.opt`. The client reads `images.client`, and the server the other four.

```lua
amux.opt.images.client = "off"
amux.opt.images.client_memory_mb = 64
```

## Clipboard

Text that amux copies, such as a selection yanked in copy mode, goes to the clipboard of the client that copied it. The client writes it to its terminal, runs a command with it, or both.

| Option    | Default | Is                                                                                         |
| --------- | ------- | ------------------------------------------------------------------------------------------ |
| `osc52`   | `true`  | Whether the client writes the text to its terminal as an OSC 52 sequence, which puts it on the clipboard of the machine the terminal runs on, over SSH too |
| `command` | none    | A program and its arguments, run on the client's machine with the text on standard input, such as `{ "wl-copy" }`, `{ "xclip", "-selection", "clipboard" }` or `{ "pbcopy" }`. Its output is thrown away, and when it can't start or fails, the error shows on the status row |

Both live under `amux.opt.clipboard`, and the client reads them, so a reload applies them to the next copy. Copied text past 1 MiB is cut off.

Most terminals take OSC 52, among them kitty, ghostty, WezTerm, foot, Alacritty and the amux app. Some, such as GNOME Terminal, ignore it, and iTerm2 only takes it once you allow it in its settings; set `command` for those. On Android, the app's terminal takes OSC 52 sequences long enough for the whole 1 MiB. Android's clipboard is the limit there: it moves the text between processes in one transaction of about 1 MB, so it can turn down a copy of a few hundred KB or more, and the app then shows a short message and leaves the clipboard as it was.

```lua
amux.opt.clipboard.command = { "wl-copy" }
```

## Mouse

The server that holds the session reads these, and a reload applies them at once. The [README](../README.md#windows-and-panes) describes what the mouse does.

| Option           | Default | Is                                                                                         |
| ---------------- | ------- | ------------------------------------------------------------------------------------------ |
| `scroll`         | `true`  | Whether the terminal reports the mouse to amux in every window, so the wheel scrolls a pane's history and dragging selects text in [copy mode](#copy-mode). With `false`, it reports the mouse only while a window has more than one pane or the active pane is in copy mode |
| `scroll_lines`   | `3`     | How many lines each turn of the wheel scrolls, in copy mode and as `Up` and `Down` keys to a program in the alternate screen |
| `escape_time_ms` | `25`    | How long a lone `Escape` that could start a mouse report waits for the rest of it          |

All of them live under `amux.opt.mouse`. On Android, a swipe on the terminal scrolls like the wheel.

```lua
amux.opt.mouse.scroll_lines = 5
amux.opt.mouse.scroll = false
```

## Frames

The server that holds the session sends each attached client at most one frame every `amux.opt.session.frame_interval_ms`, `8` by default, so a program that prints without pause costs one screen update per interval rather than one per write. A frame after a quiet spell longer than the interval goes out at once, so a keystroke is never held back. `0` sends every change as soon as the client can take it, and the most is `1000`. A reload applies it from the next frame.

```lua
amux.opt.session.frame_interval_ms = 16
```

## The status bar

| Option                | Default                  | Is                                                                 |
| --------------------- | ------------------------ | ------------------------------------------------------------------ |
| `enabled`             | `true`                   | Whether the bottom row is a status bar                             |
| `left`                | none                     | A function that replaces the `[session@server]` tag                |
| `right`               | none                     | A function that replaces the offline servers and the latency       |
| `interval_ms`         | `0`                      | Also reruns `left` and `right` every this many ms, on the clock    |
| `session_format`      | `"[{session}@{server}]"` | The tag, with `{session}` and `{server}`                           |
| `window_format`       | `" {index}:{name} "`     | Each window, with `{index}` and `{name}`                           |
| `latency_format`      | `" {latency} "`          | The latency, with `{latency}`                                      |
| `offline_format`      | `" {server} offline "`   | An offline server, with `{server}`                                 |
| `offline_count_format`| `" {count} offline "`    | The offline servers when their names don't fit, with `{count}`     |
| `hidden_marker`       | `"…"`                    | Marks windows cut from the list                                    |

All of them live under `amux.opt.status`. With `enabled = false` the session gets the whole terminal. Notices and prompts then cover the bottom row while they show, and the session redraws it after.

`left` and `right` get the same `ctx` as a binding, with `width` added, and return `nil` to keep the built-in segment, a string, or a list of spans `{ text = …, style = { … } }`. The window list stays in the middle, and a right side that doesn't fit is dropped whole. They run again only when the session, the cluster status or the width changes, or on the next `interval_ms` tick, and not while a notice shows. Each call has 50 ms, and an error shows once on the status row, after which the built-in segment is used. `amux.state()` works in them, the functions that change something don't.

```lua
amux.opt.status.interval_ms = 1000
amux.opt.status.right = function(ctx)
  return { { text = os.date(" %H:%M "), style = { fg = "black", bg = "cyan" } } }
end
```

## The theme

Each slot under `amux.opt.theme` is a style: `fg`, `bg`, `bold`, `dim`, `italic`, `underline` and `reverse`, each optional. A colour is a name (`"black"`, `"red"`, `"green"`, `"yellow"`, `"blue"`, `"magenta"`, `"cyan"`, `"white"` and their `"bright-"` forms), `"default"`, an index from 0 to 255, or `"#rrggbb"`. Assigning a table replaces the whole slot, and assigning one field changes only that field: `amux.opt.theme.status.bg = "blue"`.

| Slot                   | Styles                                                 | Default              |
| ---------------------- | ------------------------------------------------------ | -------------------- |
| `status`               | The status bar, and the base of the other `status_` slots and of status function spans | black on green |
| `status_session`       | The session tag                                        | bold                 |
| `status_active_window` | The active window                                      | bold, reverse        |
| `status_offline`       | Offline servers                                        | bold white on red    |
| `message`              | Notices and errors on the status row                   | black on yellow      |
| `prompt`               | A prompt                                               | black on yellow      |
| `prompt_label`         | The prompt's label, over `prompt`                      | bold                 |
| `tree`                 | The cluster tree                                       | plain                |
| `tree_server`          | Server rows, over `tree`                               | bold                 |
| `tree_stale`           | Sessions of offline servers, over `tree`               | dim                  |
| `tree_cursor`          | The selected row                                       | reverse              |
| `picker`               | The search lists, and the base of the other `picker_` slots | plain           |
| `picker_label`         | The label before the query, such as `project>`         | bold                 |
| `picker_cursor`        | The selected row                                       | reverse              |
| `picker_match`         | The letters that match the query                       | bold yellow          |
| `picker_detail`        | The path after each entry, the count and messages      | dim                  |
| `which_key`            | The which-key popup, and the base of the other `which_key_` slots | plain     |
| `which_key_border`     | The rule at the top of the popup, and the page         | dim                  |
| `which_key_title`      | The keys typed so far, on the rule                     | bold                 |
| `which_key_key`        | Each key                                               | bold cyan            |
| `which_key_separator`  | The separator between a key and its description        | dim                  |
| `which_key_group`      | The description of a key that switches to another table | magenta             |
| `overlay_text`         | The reconnecting overlay's text                        | bold                 |
| `overlay_border`       | The reconnecting overlay's border                      | plain                |
| `pane_border`          | Pane borders, drawn by the host                        | plain                |
| `pane_border_active`   | The active pane's border, drawn by the host            | green                |
| `copy_position`        | The `[line/total]` position in copy mode, drawn by the host | black on yellow |
| `copy_selection`       | The selection in copy mode, drawn by the host          | reverse              |
| `copy_prompt`          | The search prompt and its messages in copy mode, drawn by the host | black on yellow |

The pane border and copy mode slots come from the server's config, since the host draws them. Everything else comes from the client's.

## The Android keyboard

`amux.opt.android.keyboard` is the landscape keyboard of the Android app, see [The landscape keyboard](../README.md#the-landscape-keyboard). The client and the server ignore it. The app reads it with `amux config keyboard`, which loads `init.lua` the way the client does, so `amux.process` is `"client"` there. Its keys are named the way `amux.keymap` names them (see [Keys](#keys)), such as `"PageUp"` or `"C-c"`, and `false` stands for a key or row that shows the `base` layer's, since a Lua list can't hold `nil`.

```lua
local keyboard = amux.opt.android.keyboard
keyboard.width.right = 30
keyboard.layers.base.left[2][3] = { key = "e", hold = { "E", "é", "è" } }
keyboard.layers.nav.right[2][1] = { prefix = "c", label = "new" }
```

## Hooks

`amux.on(event, fn)` runs `fn(event)` in the server when something happens there. The client accepts `amux.on` and never runs it. `event.event` is the event's name, and the other fields are:

| Event             | Fields                                                                  |
| ----------------- | ----------------------------------------------------------------------- |
| `server_started`  | `server`                                                                |
| `session_created` | `session`                                                               |
| `session_closed`  | `session`                                                               |
| `session_renamed` | `session`, `old`                                                        |
| `window_created`  | `session`, `window`, `name`                                             |
| `window_closed`   | `session`, `window`, `name`                                             |
| `pane_exited`     | `session`, `window`, `pane`, and `status` or `signal` when known        |
| `client_attached` | `session`, `origin` (`"local"` or `"peer"`), `clients`                  |
| `client_detached` | `session`, `origin`, `clients`                                          |
| `peer_online`     | `peer`                                                                  |
| `peer_offline`    | `peer`                                                                  |

Inside a hook, these read and change the server's own sessions:

| Function                                            | Does                                                              |
| --------------------------------------------------- | ----------------------------------------------------------------- |
| `amux.server_name()`                                | This server's name                                                |
| `amux.sessions()`                                   | Every session: `{ name, clients, windows, project, branch }`, each window `{ index, name, panes }` |
| `amux.session(name)`                                | One session, or `nil`                                             |
| `amux.new_window { session = … }`                   | Opens a window                                                    |
| `amux.rename_window { session = …, window = …, name = … }` | Renames a window                                           |
| `amux.send_keys { session = …, window = …, pane = …, keys = … }` | Types `keys` into a pane. `window` and `pane` default to the active ones |
| `amux.rename_session(session, name)`                | Renames a session                                                 |
| `amux.kill_session(session)`                        | Kills a session                                                   |

The changes are queued and applied after the hook returns. A change that fails, such as a session that is gone, is logged. Hooks run one at a time on their own thread, so a slow hook delays later hooks and never the server. Each has one second, and an error or a hook that runs past it is logged to the server log (`<socket>.log`) and the next event still runs. Events caused by a hook's changes run hooks too, down to three levels, which stops a `window_created` hook that opens a window from looping.

```lua
amux.on("session_created", function(event)
  amux.log("new session " .. event.session)
  amux.send_keys { session = event.session, keys = "git status\r" }
end)
```
