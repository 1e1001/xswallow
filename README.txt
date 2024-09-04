xswallow

Correctly implemented window swallowing for X11.  Meant to replicate Liupold's
pidswallow library, but rewritten in C to properly handle some edge cases.

What is window swallowing:
  When you run a graphical program from the terminal, the terminal window will
  get visually replaced by the graphical program, and the other way around when
  exiting the program.  I think this came from plan9 but I'm not sure on that.

Fixes from the original pidswallow include:
- Only vomiting after all the child's windows are closed
- Correctly handling positioning with window frames
- Using in-memory data instead of files in /tmp to store swallow status

Simple makefile building, should be pretty easy to get working on your system.
Dependencies:
- A reasonable computer
- xcb, xcb-util-errors

Configure by changing the following env vars
- TERMINAL: process name for your terminal emulator
- XSWALLOW_TERMINALS: :-seperated additional terminal emulators
- XSWALLOW_IMMUNE: :-seperated list of programs to be immune to being swallowed

I recommend LD_PRELOAD-ing the _NET_WM_PID hack, as it allows every type of
program to be swallowed.
