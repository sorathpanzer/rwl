# IPC & status reference

rwl exposes two related mechanisms:

1. A **command socket** (`ipc` feature) for controlling and querying the running
   compositor via `rwl msg …`.
2. A **status stream** (dwl-compatible) written to a pipe, consumed by a status
   bar.

Source: [`src/features/ipc/`](../src/features/ipc/) (socket server + `rwl msg`
client) and [`src/ipc.rs`](../src/ipc.rs) (status formatting).

---

## The command socket

When the `ipc` feature is enabled, the compositor binds a Unix socket at
`$XDG_RUNTIME_DIR/rwl.sock` and exports its path to every spawned child as the
`RWL_SOCK` environment variable. The same `rwl` binary is the client: when
invoked as `rwl msg …` it connects to `RWL_SOCK`, sends the command, and (for
queries) reads the response — all *before* any compositor state is created.

```
rwl msg <command> [args…]
```

If `RWL_SOCK` is unset or the compositor is not running, `rwl msg` prints an
error to stderr.

### WM commands

| Command | Args | Effect |
|---------|------|--------|
| `status` | `[<output>]` | Print current status once (see [format](#status-format)). Query. |
| `subscribe` | `[<output>]` | Stream status on every state change until disconnect. |
| `clients` | | Print a JSON array of all windows (see [structured events](#structured-events-clients--watch)). Query. |
| `watch` | `[<kinds>]` | Stream newline-delimited JSON events until disconnect. Optional comma/space-separated kind filter. |
| `view` | `[<output>] <tagmask>` | View the given tag mask. |
| `toggleview` | `[<output>] <tagmask>` | Toggle a tag into/out of the view. |
| `setlayout` | `[<output>] <index>` | Select a layout by index. |
| `tag` | `<tagmask>` | Move focused window to the tag mask. |
| `toggletag` | `<tagmask>` | Toggle a tag on the focused window. |
| `focusstack` | `<1\|-1>` | Move focus up/down the stack. |
| `incnmaster` | `<delta>` | Change master-window count. |
| `setmfact` | `<delta>` | Change master-area fraction. |
| `cyclelayout` | | Cycle to the next layout. |
| `zoom` | | Swap focused window with master. |
| `viewprev` | | View the previously selected tag. |
| `viewnextocctag` | `<1\|-1>` | Jump to next/prev occupied tag. |
| `killclient` | | Close the focused window. |
| `togglefloating` | | Toggle floating on the focused window. |
| `togglefullscreen` | | Toggle fullscreen. |
| `togglepassthrough` | | Toggle keybinding passthrough. |
| `focusmon` | `<1\|-1>` | Focus next/prev monitor. |
| `tagmon` | `<1\|-1>` | Send focused window to next/prev monitor. |
| `chvt` | `<n>` | Switch to virtual terminal `n`. |
| `viewtag_spawn` | `<tagmask>` | View a tag, auto-spawning its command on first view. |
| `spawn` | `<cmd> [args…]` | Launch a command. |
| `focusurgent` | | Focus the urgent window. |
| `reloadconfig` | | Re-read `config.lua`. |
| `quit` | | Exit the compositor. |
| `info` | `[title\|class\|appid\|tags\|fullscreen\|floating]` | Query a property of the focused window. Query. |
| `togglebar` | | Show/hide the bar. (`bar`) |
| `barprompt` | | Open the bar prompt. (`bar`) |
| `togglegaps` | | Toggle gaps. (`gaps`) |
| `lock` | | Engage the native screen locker. (`lock`) |
| `wallpaper` | `<path> [fill\|fit\|stretch\|center]` / `off` | Set or clear the desktop wallpaper at runtime. (`wallpaper`) |

**`<output>`** for per-output commands (`view`, `toggleview`, `setlayout`,
`status`, `subscribe`) may be `all`, `selected`, or a named output such as
`eDP-1`. When omitted, the selected output is used.

**`status` / `info`** are *queries*: the client writes the command, half-closes
the write side, and reads the reply back. **`subscribe`** keeps the connection
open and prints each pushed status update line-by-line.

### Bar commands (`bar` feature)

Dash-prefixed commands drive the embedded status bar (or any dwlb-compatible bar
whose sockets live in `$XDG_RUNTIME_DIR/dwlb/`, named `dwlb-*`). Multiple bar
commands can be chained in one invocation.

| Command | Args | Effect |
|---------|------|--------|
| `-status` | `<output> <text>` | Set the status text. |
| `-title` | `<output> [<secs>] [<#rrggbb>] <text>` | Show a title notification (optional duration + colour). |
| `-prompt` | `<output>` | Open the bar prompt. |
| `-show` | `<output>` | Show the bar. |
| `-hide` | `<output>` | Hide the bar. |
| `-toggle-visibility` | `<output>` | Toggle bar visibility. |
| `-set-top` | `<output>` | Move the bar to the top. |
| `-set-bottom` | `<output>` | Move the bar to the bottom. |
| `-toggle-location` | `<output>` | Toggle top/bottom. |

`<output>` accepts `all`, `selected`, or a named output.

Examples:

```sh
rwl msg -status all "$(date '+%H:%M')"
rwl msg -title eDP-1 5 '#ff5555' "Build finished"
rwl msg -toggle-visibility all
```

---

## Structured events (`clients` / `watch`)

For scripting (window switchers, activity loggers, external panels), the socket
also speaks JSON. Source: [`src/features/ipc/event.rs`](../src/features/ipc/event.rs).

**`rwl msg clients`** prints a one-shot JSON array of every window:

```json
[{"app_id":"foot","title":"~","tags":2,"monitor":0,"floating":false,
  "fullscreen":false,"focused":true,"x":0,"y":27,"w":960,"h":1053}, …]
```

**`rwl msg watch [kinds]`** streams newline-delimited JSON event objects, one per
line, flushed immediately. With no argument every event is delivered; otherwise
pass a comma/space-separated subset (e.g. `rwl msg watch window focus`).

| Event kind | Shape |
|------------|-------|
| `window` | `{"event":"window","action":"open"\|"close","window":{…}}` |
| `focus` | `{"event":"focus","window":{…}\|null}` |
| `title` | `{"event":"title","window":{…}}` |
| `tag` | `{"event":"tag","monitor":<i>,"selected":<mask>,"occupied":<mask>}` |

The `window` object is the same shape emitted by `clients`. Example — a live
window menu with `fuzzel`:

```sh
rwl msg clients | jq -r '.[] | "\(.tags)\t\(.title)"' | fuzzel --dmenu
```

```sh
rwl msg watch focus | while read -r ev; do
    echo "$ev" | jq -r '.window.app_id // "none"'
done
```

---

## Status format

[`src/ipc.rs`](../src/ipc.rs) writes status in the dwl `printstatus()` format so
existing dwl bars (sombar, dwl-bar, waybar via the dwl IPC protocol, and the
embedded bar) understand it. Seven lines are emitted **per monitor** on each
update:

```text
<name> title <focused_title>
<name> appid <focused_appid>
<name> fullscreen <0|1>
<name> floating <0|1>
<name> selmon <0|1>
<name> tags <occupied> <selected> <focused_client_tags> <urgent>
<name> layout <symbol>
```

- `<name>` is the output name (e.g. `eDP-1`).
- The `tags` line carries four bitmasks: occupied tags, selected tags, the
  focused client's tags, and urgent tags.
- `<symbol>` is the current layout's symbol (`[]=`, `[M]`, `||`, `>>>`, `[@]`,
  `(B)`, `|-|`).

This stream is written to the `ipc_out` pipe. It is delivered on stdin to:

- the `-s <cmd>` startup command,
- the `bar_cmd` from config,
- or the **embedded bar** thread (when the `bar` feature is on and no external
  bar was launched),

whichever is active. `rwl msg subscribe` delivers the same content over the
command socket to arbitrary clients.

---

## Relationship to keybinding actions

Most WM commands map one-to-one to a keybinding `Action` (see
[CONFIGURATION.md](CONFIGURATION.md#actions)). Whether an action is triggered by
a key, a pointer button, or `rwl msg`, it flows through the same
`Rwl::dispatch()` in [`src/actions.rs`](../src/actions.rs). This means anything
you can bind to a key you can also drive from a script via `rwl msg`, and
vice-versa.
