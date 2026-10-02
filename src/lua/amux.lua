---@meta amux

---@alias amux.Process "client"|"server"

---A key as tmux writes it, such as "C-b", "M-h", "S-Left", "%" or "PageUp"
---@alias amux.Key string

---@alias amux.ColorName
---| "default"
---| "black"
---| "red"
---| "green"
---| "yellow"
---| "blue"
---| "magenta"
---| "cyan"
---| "white"
---| "bright-black"
---| "bright-red"
---| "bright-green"
---| "bright-yellow"
---| "bright-blue"
---| "bright-magenta"
---| "bright-cyan"
---| "bright-white"

---A colour name, "default", an index from 0 to 255, or "#rrggbb"
---@alias amux.Color amux.ColorName|integer|string

---@class amux.Style
---@field fg? amux.Color
---@field bg? amux.Color
---@field bold? boolean
---@field dim? boolean
---@field italic? boolean
---@field underline? boolean
---@field reverse? boolean

---@class amux.Span
---@field text string
---@field style? amux.Style

---nil keeps the built-in segment
---@alias amux.StatusSegment string|amux.Span[]|nil

---@class amux.Window
---@field index integer
---@field name string
---@field panes integer
---@field active boolean

---A read-only snapshot of the client
---@class amux.Context
---@field session string The attached session's name
---@field server string The server that holds the session
---@field local_server string This client's own server
---@field window_index? integer The active window, when the session has sent it
---@field window_name? string The active window's name, when the session has sent it
---@field panes? integer The active window's pane count, when the session has sent it
---@field windows amux.Window[] Every window of the session
---@field latency_ms? integer The latency to a remote session's server, when it is known
---@field offline string[] The names of the servers that are offline

---@class amux.StatusContext: amux.Context
---@field width integer The terminal width

---@class amux.SessionWindow
---@field index integer
---@field name string
---@field panes integer

---@class amux.Session
---@field name string
---@field clients integer
---@field windows amux.SessionWindow[]
---@field project? string
---@field branch? string

---@alias amux.ActionName
---| "detach"
---| "send_prefix"
---| "new_window"
---| "next_window"
---| "previous_window"
---| "select_window"
---| "split_pane"
---| "next_pane"
---| "select_pane"
---| "kill_pane"
---| "kill_window"
---| "rename_window"
---| "rename_session"
---| "cluster_tree"
---| "search_projects"
---| "search_worktrees"
---| "switch_table"
---| "reload_config"
---| "which_key"

---An action: its name, or a table such as { split_pane = "left-right" }
---@alias amux.Action amux.ActionName|table<amux.ActionName, any>

---@alias amux.PromptAction
---| "submit"
---| "cancel"
---| "delete_backward"
---| "delete_forward"
---| "delete_line"
---| "cursor_left"
---| "cursor_right"
---| "cursor_start"
---| "cursor_end"

---@alias amux.TreeAction
---| "down"
---| "up"
---| "top"
---| "bottom"
---| "collapse"
---| "expand"
---| "pick"
---| "cancel"

---@alias amux.PickerAction
---| "down"
---| "up"
---| "pick"
---| "cancel"
---| "delete_backward"
---| "delete_word"
---| "delete_line"

---@alias amux.Binding amux.Action|fun(ctx: amux.Context)|amux.PromptAction|amux.TreeAction|amux.PickerAction

---@alias amux.KeymapTable
---| "root"
---| "prefix"
---| "prompt"
---| "tree"
---| "picker"
---| "search"
---| string

---@class amux.KeymapOpts
---@field desc? string Describes the key in the which-key popup

---@class amux.action
---@field detach fun(): amux.Action Detaches the client
---@field send_prefix fun(): amux.Action Sends the prefix key to the pane
---@field new_window fun(): amux.Action Opens a window
---@field next_window fun(): amux.Action Selects the next window
---@field previous_window fun(): amux.Action Selects the previous window
---@field select_window fun(index: integer): amux.Action Selects the window with this number
---@field split_pane fun(split: "left-right"|"top-bottom"): amux.Action Splits the active pane
---@field next_pane fun(): amux.Action Selects the next pane
---@field select_pane fun(direction: "left"|"right"|"up"|"down"): amux.Action Selects the pane in this direction
---@field kill_pane fun(): amux.Action Kills the active pane
---@field kill_window fun(): amux.Action Kills the active window
---@field rename_window fun(): amux.Action Opens the rename window prompt
---@field rename_session fun(): amux.Action Opens the rename session prompt
---@field cluster_tree fun(): amux.Action Opens the cluster tree
---@field search_projects fun(): amux.Action Opens the list of git repositories in the project directories
---@field search_worktrees fun(): amux.Action Opens the list of worktrees of the attached session's project
---@field switch_table fun(name: string): amux.Action Reads the next key from this table
---@field reload_config fun(): amux.Action Reloads the config here and on this machine's server
---@field which_key fun(name: string): amux.Action Shows the keys of this table, then reads the next key from it

---@alias amux.EventName
---| "session_created"
---| "session_closed"
---| "session_renamed"
---| "window_created"
---| "window_closed"
---| "pane_exited"
---| "client_attached"
---| "client_detached"
---| "peer_online"
---| "peer_offline"
---| "server_started"

---@class amux.event.SessionCreated
---@field event "session_created"
---@field session string

---@class amux.event.SessionClosed
---@field event "session_closed"
---@field session string

---@class amux.event.SessionRenamed
---@field event "session_renamed"
---@field session string
---@field old string

---@class amux.event.WindowCreated
---@field event "window_created"
---@field session string
---@field window integer
---@field name string

---@class amux.event.WindowClosed
---@field event "window_closed"
---@field session string
---@field window integer
---@field name string

---@class amux.event.PaneExited
---@field event "pane_exited"
---@field session string
---@field window integer
---@field pane integer
---@field status? integer
---@field signal? string

---@class amux.event.ClientAttached
---@field event "client_attached"
---@field session string
---@field origin "local"|"peer"
---@field clients integer

---@class amux.event.ClientDetached
---@field event "client_detached"
---@field session string
---@field origin "local"|"peer"
---@field clients integer

---@class amux.event.PeerOnline
---@field event "peer_online"
---@field peer string

---@class amux.event.PeerOffline
---@field event "peer_offline"
---@field peer string

---@class amux.event.ServerStarted
---@field event "server_started"
---@field server string

---@alias amux.Event
---| amux.event.SessionCreated
---| amux.event.SessionClosed
---| amux.event.SessionRenamed
---| amux.event.WindowCreated
---| amux.event.WindowClosed
---| amux.event.PaneExited
---| amux.event.ClientAttached
---| amux.event.ClientDetached
---| amux.event.PeerOnline
---| amux.event.PeerOffline
---| amux.event.ServerStarted

---@class amux.ServerConfig
---@field address string ssh://[user@]host[:port], exec:<command>, tcp://host:port or lan://<server id>
---@field amux_path? string The amux on the other machine, for ssh:// addresses
---@field socket? string The socket name on the other machine, this server's own by default

---@class amux.ProjectConfig
---@field default_server? string Where amux new puts this project's sessions without --on
---@field worktrees_dir? string Where its worktrees go instead of <checkout>/../<project>-worktrees

---A key name such as "Escape", a character, "layer:<name>", "" for a gap, false for the base layer's key, or a key table
---@alias amux.KeySlot string|false|amux.KeyTable

---A list of keys, or false for the base layer's row
---@alias amux.KeyRow amux.KeySlot[]|false

---@class amux.KeyTable
---@field key? string A key name or a character
---@field text? string Text the key types; Ctrl and Alt apply to each of its characters
---@field send? string Characters the key sends to the terminal as they are
---@field prefix? string Keys to send after amux's prefix, such as "c" or "s p"
---@field label? string What the key shows instead
---@field shift? string What the key types with Shift
---@field width? number The key's share of its row, 1 unless set
---@field repeats? boolean Repeats the key while it is held
---@field hold? amux.KeySlot|amux.KeySlot[] What holding the key does; a list opens a popup to pick from
---@field taps? amux.KeySlot[] What two, three or more quick taps send instead

---@class amux.KeyboardLayer
---@field left? amux.KeyRow[]
---@field right? amux.KeyRow[]

---@class amux.opt
---@field name? string This server's name, the hostname by default. Changing it needs a restart
---@field projects_dir string Where new projects are checked out
---@field servers table<string, amux.ServerConfig> The peers to link to, by name
---@field projects table<string, amux.ProjectConfig> Per-project settings, by project name
---@field prefix amux.Key The prefix key
---@field escape_time_ms integer How long a lone Escape waits for the rest of a sequence
---@field notice_ms integer How long notices and errors stay on the status row
---@field status amux.opt.status
---@field theme amux.opt.theme
---@field tree amux.opt.tree
---@field which_key amux.opt.which_key
---@field search amux.opt.search
---@field pane amux.opt.pane
---@field window amux.opt.window
---@field session amux.opt.session
---@field borders amux.opt.borders
---@field mouse amux.opt.mouse
---@field cluster amux.opt.cluster
---@field discovery amux.opt.discovery
---@field lan amux.opt.lan
---@field worktrees amux.opt.worktrees
---@field reload amux.opt.reload
---@field android amux.opt.android

---@class amux.opt.status
---@field enabled boolean Whether the bottom row is a status bar
---@field left? fun(ctx: amux.StatusContext): amux.StatusSegment Replaces the [session@server] tag
---@field right? fun(ctx: amux.StatusContext): amux.StatusSegment Replaces the offline servers and the latency
---@field interval_ms integer Also reruns left and right every this many ms, 0 for never
---@field session_format string The tag, with {session} and {server}
---@field window_format string Each window, with {index} and {name}
---@field latency_format string The latency, with {latency}
---@field offline_format string An offline server, with {server}
---@field offline_count_format string The offline servers when their names don't fit, with {count}
---@field hidden_marker string Marks windows cut from the list

---@class amux.opt.theme
---@field status amux.Style The status bar, and the base of the other status_ slots
---@field status_session amux.Style The session tag
---@field status_active_window amux.Style The active window
---@field status_offline amux.Style Offline servers
---@field message amux.Style Notices and errors on the status row
---@field prompt amux.Style A prompt
---@field prompt_label amux.Style The prompt's label
---@field tree amux.Style The cluster tree
---@field tree_server amux.Style Server rows of the tree
---@field tree_stale amux.Style Sessions of offline servers
---@field tree_cursor amux.Style The selected row of the tree
---@field picker amux.Style The search lists, and the base of the other picker_ slots
---@field picker_label amux.Style The label before the query
---@field picker_cursor amux.Style The selected row of a list
---@field picker_match amux.Style The letters that match the query
---@field picker_detail amux.Style The path after each entry, the count and messages
---@field which_key amux.Style The which-key popup, and the base of the other which_key_ slots
---@field which_key_border amux.Style The rule at the top of the popup
---@field which_key_title amux.Style The keys typed so far
---@field which_key_key amux.Style Each key
---@field which_key_separator amux.Style Between a key and its description
---@field which_key_group amux.Style A key that switches to another table
---@field overlay_text amux.Style The reconnecting overlay's text
---@field overlay_border amux.Style The reconnecting overlay's border
---@field pane_border amux.Style Pane borders, from the server's config
---@field pane_border_active amux.Style The active pane's border, from the server's config

---@class amux.opt.tree
---@field indent string
---@field expanded_marker string
---@field collapsed_marker string
---@field leaf_marker string
---@field detail_gap string

---@class amux.opt.which_key
---@field enabled boolean Whether the popup opens on its own
---@field delay_ms integer How long the client waits for the next key before the popup opens
---@field separator string Between a key and its description
---@field group_marker string Before the description of a key that switches to another table

---@class amux.opt.search
---@field project_dirs string[] The directories to look for repositories in
---@field project_depth integer How many levels below each directory to look

---@class amux.opt.pane
---@field shell? string[] The command a new pane runs, your login shell by default
---@field term string TERM in new panes
---@field scrollback integer Lines of scrollback per pane
---@field env table<string, string> Variables set in new panes
---@field strip_env string[] Prefixes of variables removed from new panes

---@class amux.opt.window
---@field base_index integer The number of the first window
---@field name? string The name of new windows

---@class amux.opt.session
---@field base_index integer
---@field clash_format string The name of a session whose name is taken, with {base} and {n}
---@field clash_start integer The first {n} of clash_format
---@field activity_interval_ms integer

---@class amux.opt.borders
---@field horizontal string
---@field vertical string
---@field top_left string
---@field top_right string
---@field bottom_left string
---@field bottom_right string
---@field left_tee string
---@field right_tee string
---@field top_tee string
---@field bottom_tee string
---@field cross string

---@class amux.opt.mouse
---@field escape_time_ms integer

---@class amux.opt.cluster
---@field ping_interval_ms integer
---@field missed_pings integer
---@field handshake_timeout_ms integer
---@field connect_timeout_ms integer
---@field backoff_min_ms integer
---@field backoff_max_ms integer
---@field max_unverified_backoff_ms integer
---@field address_expiry_hours integer
---@field status_interval_ms integer
---@field ssh amux.opt.cluster.ssh

---@class amux.opt.cluster.ssh
---@field program string
---@field options string[]
---@field default_amux_path string

---@class amux.opt.discovery
---@field tailscale boolean Link to the machines on the tailnet and listen on the tailnet address
---@field lan boolean Find servers over mDNS, listen on the LAN and allow amux pair
---@field tailscale_tags string[] Tailscale tags whose machines count as yours
---@field tailscale_port? integer The port of the tailnet listener
---@field tailscale_program string
---@field interval_ms integer

---@class amux.opt.lan
---@field port integer The port of the LAN listener, 0 for a free one
---@field mdns_service string
---@field pairing_window_ms integer

---@class amux.opt.worktrees
---@field suffix string
---@field fetch_timeout_ms integer

---@class amux.opt.reload
---@field watch boolean Reload the config when init.lua, servers.lua or a module in lua/ changes
---@field interval_ms integer How often the config files are checked for changes

---@class amux.opt.android
---@field keyboard amux.opt.android.keyboard The Android app's landscape keyboard

---@class amux.opt.android.keyboard
---@field width amux.opt.android.keyboard.width
---@field hold_ms integer How long a key is held before its hold happens
---@field taps_ms integer How long a key with taps waits for another tap
---@field layers table<string, amux.KeyboardLayer>

---@class amux.opt.android.keyboard.width
---@field left number Percent of the screen's width, from 5 to 45
---@field right number Percent of the screen's width, from 5 to 45

---@class amux.PromptSpec
---@field label? string
---@field initial? string
---@field on_submit fun(text: string, ctx: amux.Context)

---@class amux.NewWindowSpec
---@field session string

---@class amux.RenameWindowSpec
---@field session string
---@field window integer
---@field name string

---@class amux.SendKeysSpec
---@field session string
---@field window? integer The active window unless set
---@field pane? integer The active pane unless set
---@field keys string

---@class amux
---@field opt amux.opt Every setting
---@field action amux.action Action constructors for bindings
---@field process amux.Process Which process runs this config
amux = {}

amux.keymap = {}

---Binds keys in a table, replacing what they were bound to
---@param table amux.KeymapTable
---@param keys string One key, or keys separated by spaces through submaps
---@param binding amux.Binding
---@param opts? amux.KeymapOpts
function amux.keymap.set(table, keys, binding, opts) end

---Unbinds keys, an error when they are not bound
---@param table amux.KeymapTable
---@param keys string
function amux.keymap.del(table, keys) end

---The action or function bound to keys, or nil
---@param table amux.KeymapTable
---@param keys string
---@return amux.Action|function|nil
function amux.keymap.get(table, keys) end

---Unbinds every key in a table, and removes a custom table
---@param table amux.KeymapTable
function amux.keymap.clear(table) end

---Registers a hook, which only the server runs
---@overload fun(event: "session_created", fn: fun(event: amux.event.SessionCreated))
---@overload fun(event: "session_closed", fn: fun(event: amux.event.SessionClosed))
---@overload fun(event: "session_renamed", fn: fun(event: amux.event.SessionRenamed))
---@overload fun(event: "window_created", fn: fun(event: amux.event.WindowCreated))
---@overload fun(event: "window_closed", fn: fun(event: amux.event.WindowClosed))
---@overload fun(event: "pane_exited", fn: fun(event: amux.event.PaneExited))
---@overload fun(event: "client_attached", fn: fun(event: amux.event.ClientAttached))
---@overload fun(event: "client_detached", fn: fun(event: amux.event.ClientDetached))
---@overload fun(event: "peer_online", fn: fun(event: amux.event.PeerOnline))
---@overload fun(event: "peer_offline", fn: fun(event: amux.event.PeerOffline))
---@overload fun(event: "server_started", fn: fun(event: amux.event.ServerStarted))
---@param event amux.EventName
---@param fn fun(event: amux.Event)
function amux.on(event, fn) end

---This machine's hostname
---@return string
function amux.hostname() end

---Writes its arguments, tab-separated, to the server log
---@param ... any
function amux.log(...) end

---Shows a message on the status row, client bindings only
---@param message string
function amux.notify(message) end

---Runs an action as if its key was pressed, client bindings only
---@param action amux.Action
function amux.run(action) end

---Types keys into the active pane in a client binding, or into a pane of a session in a hook
---@overload fun(spec: amux.SendKeysSpec)
---@param keys string
function amux.send_keys(keys) end

---Switches this client to a target such as "notes@laptop" or "work:1", client bindings only
---@param target string
function amux.switch(target) end

---Opens a prompt, client bindings only
---@param spec amux.PromptSpec
function amux.prompt(spec) end

---The same snapshot as ctx, client only
---@return amux.Context
function amux.state() end

---This server's name, hooks only
---@return string
function amux.server_name() end

---Every session of this server, hooks only
---@return amux.Session[]
function amux.sessions() end

---One session of this server, or nil, hooks only
---@param name string
---@return amux.Session?
function amux.session(name) end

---Opens a window, hooks only
---@param spec amux.NewWindowSpec
function amux.new_window(spec) end

---Renames a window, hooks only
---@param spec amux.RenameWindowSpec
function amux.rename_window(spec) end

---Renames a session, hooks only
---@param session string
---@param name string
function amux.rename_session(session, name) end

---Kills a session, hooks only
---@param session string
function amux.kill_session(session) end
