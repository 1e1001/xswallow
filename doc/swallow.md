# Window swallowing
Correctly implemented window swallowing. Replacement for [`pidswallow`](https://github.com/liupold/pidswallow).

## What is window swallowing
When you run a graphical program from the terminal, the terminal window will get visually replaced by the graphical program, and the other way around when exiting the program. I think this came from plan9 OS but I'm not sure on that.

## Fixes from the original `pidswallow`
- Only vomiting after *all* the child's windows are closed
- Correctly handling positioning windows
- Tracks more properties of child windows (e.g. maximized / minimized state)
- Using in-memory data instead of files in `/tmp` to store swallow status

I recommend `LD_PRELOAD`-ing [the `_NET_WM_PID` hack](<https://github.com/deepfire/ld-preload-xcreatewindow-net-wm-pid/>), as it allows programs that don't support EWMH (e.g. anything using raw X) to be captured.

## Extra commands
`xextra swallow:toggle` can be used to toggle a window's swallow status

## Platform support
Depends on `/proc/{pid}/status` existing, so it's most likely Linux only. This should work on any window manager that supports ICCCM and EWMH. I don't have any graphical linux computers other than my laptop, so feel free to test this on your own window manager to report bugs, particularly if more/less window "geometry" should be saved.
- `openbox`: working
