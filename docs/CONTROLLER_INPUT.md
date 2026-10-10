# Controller input

Updated 2026-10-10. Review candidate on `codex/controller-support`.

The viewer reads the first connected gamepad through Bevy's standard gamepad
API. Stick axes use a radial 0.18 dead zone. Gameplay bindings load their
button bytes from `FalloutPrefs.ini`'s `[Controls]` section, falling back to
FalloutNV.exe 1.4.0.525's defaults (`viewer/src/controls.rs`). A newly
connected pad feeds walking and camera look; mapped buttons feed combat,
interaction, dialogue, Pip-Boy and game menus. V.A.T.S. and the companion
wheel use the sticks and buttons while their menus are open.

On macOS, the viewer also reads Apple's Game Controller framework's extended
gamepad profile when available, then uses the same shared button-edge and
stick-shaping code. Linux and other platforms continue through Bevy/Gilrs;
they can exercise the shared controls but do not run the Apple-specific path.

In lockpicking, the left stick turns the cylinder and the right stick moves
the pick horizontally. X forces the lock and B leaves; the controller
tutorial (ID 0x1C) is selected while a pad is connected. The viewer maps full
right-stick deflection to one menu width per second. The original game's pad
cursor sensitivity was not traced, so this rate needs a hands-on check.

The viewer draws a smoothed FPS counter in the top-right corner using real
frame time. The `--fps` option still prints its more detailed timing report
to the terminal.

For a combat-input check, `--controller-test` gives the in-memory session
1,000,000 health and carry weight, restores health each frame, adds one of
every valid loaded weapon and 1,000 of every loaded ammo type. It does not
write to the game installation. The viewer's normal save command can save the
test state if used, so launch without this option for normal play.

## Current evidence and gaps

- The original Bevy/Gilrs controller branch passed `cargo check` on macOS.
- The new Apple-framework fallback has not been built or verified with a
  physical controller. Linux's Bevy/Gilrs path has not been checked on Linux.
- The lockpick analog sensitivity and complete controller behavior remain
  unverified on a physical pad.
- Rumble is not implemented. The viewer currently uses one connected pad.

**Next action:** test movement, look, combat, interaction, menus, Pip-Boy,
V.A.T.S. and lockpicking on physical controllers on macOS and Linux; adjust
the platform-specific input path and sensitivity from those results.
