# `xswallow`
Correctly implemented window swallowing for X. Replacement for [`pidswallow`](https://github.com/liupold/pidswallow), but rewritten in ~~C~~ Rust to properly handle some edge cases and run faster.

## What is window swallowing
When you run a graphical program from the terminal, the terminal window will get visually replaced by the graphical program, and the other way around when exiting the program. I think this came from plan9 OS but I'm not sure on that.

## Fixes from the original `pidswallow`
- Only vomiting after *all* the child's windows are closed
- Correctly handling positioning windows (mostly? `mpv` doesn't work quite right)
- Tracks more properties of child windows (e.g. maximized / minimized state)
- Using in-memory data instead of files in `/tmp` to store swallow status

## Installation
Not on crates.io, `cargo install` with `--git` or `--path`

## Configuration
Currently configuration is the same as the C version, and based on environment variables:
- `TERMINAL`: process name for your terminal emulator
- `XSWALLOW_TERMINALS`: `:`-separated additional terminal emulators
- `XSWALLOW_IMMUNE`: `:`-separated list of programs to be immune to being swallowed

I recommend `LD_PRELOAD`-ing [the `_NET_WM_PID` hack](<https://github.com/deepfire/ld-preload-xcreatewindow-net-wm-pid/>), as it allows programs that don't support EWMH (e.g. anything using raw X) to be captured.

## Platform support
Depends on:
- [`xcb`](https://docs.rs/xcb) library
- [`std::os::unix`](https://doc.rust-lang.org/std/os/unix) module
- `/proc/{pid}/status` existing

So it's most likely Linux only. This should work on any window manager that supports ICCCM and EWMH. I don't have any graphical linux computers other than my laptop, so feel free to test this on your own window manager to report bugs, particularly if more/less window "geometry" should be saved.
- `openbox`: working

## Copyright
©2024 1e1001, licensed under the EUPL

See [`LICENSE`](./LICENSE) for the english text, or [the EU's official website](https://joinup.ec.europa.eu/collection/eupl/eupl-text-eupl-12) for translations

## Changelog
- 1.0.0: Initial Rust version, port of the C version with bugs fixed along the way

## To-do list
- replace env vars with a config file
- command-line tool for real-time changes (e.g. temporarily unswallowing a parent)
- more than xswallow?
  - xbelld without audio underrun bug
  - screen dimmer overlay
- demo videos
