# ED-E My Love: Primm start

Quest `vDialogueEDE` (`001572E8`), quest script `vDialogueEDESCRIPT`
(`00157F1F`), disabled-robot script `EDEScriptDisabled` (`001572E0`). This
route is **PARTIAL** and stays opt-in (`pwsh -File scripts/acceptance.ps1
-Routes ede -Data <Data>`). It covers the quest's Primm start through the first
audio log. A separate probe also reached Ignacio's ARCHIMEDES INFO and the
quest's “Two EDE logs heard” message after six in-game waits (132 hours). The
probe did not capture the second log's dialogue or voice, so its playback is
still unverified. A follow-up scheduled Escape after Ignacio's result, but the
dialogue remained open at 500 seconds. This has not been compared side by side
with Fallout: New Vegas. The revised PowerShell acceptance wrapper was not run
on Fedora because `pwsh` is unavailable; the equivalent viewer route was run
directly.

## Acceptance route

The route starts in `PrimmNashResidence` beside `NVCompanionEDEDisabledREF`
(`001572E6`). It adds 100 Repair to the test player, activates the disabled
robot with `--use`, and answers the repair message buttons. The record's own
`EDEScriptDisabled` handles repair: its `OnActivate` presents `EDERepair00`,
then the repair path presents `EDERepair01`; the GameMode script starts
`vDialogueEDE` when the player chooses “Run the repair.” It moves/enables
`EDE1Ref` and runs its greeting. The route selects the offered recruitment
INFO `001579C3` (“I think we should travel together.”), whose result script
sets `VNPCFollowers.bEDEHired`.

The harness moves the player to `OldLadyGibsonRef` and starts her conversation.
Her greeting INFO `000841E7` has the `bEDEHired` condition and its result script
sets `vDialogueEDE` stage 10. The stage script sees `iLogsPlayed == 0`, starts
`EDEDialoguePackage02`, and displays objective 5. That package plays the first
log, INFO `00157F17`; its result script advances `iLogsPlayed` to 1. The route
stops after that log. It does not directly start the quest, set quest/follower
variables, set a stage or objective, or advance `GameDaysPassed`.

The explicit test inputs are the Repair skill boost, the simulated E press,
the dialogue topic selection and `Log Off`, the move to Gibson, and starting
Gibson's conversation. The repair choices use buttons `1,1,1`: the ESM's two
activation blocks show the initial repair message twice, followed by the “Run
the repair” choice. The route waits 80 seconds to let the first log package
finish, then stops before a second-log trigger.

A separate probe waited six times through the in-game wait menu (132 hours),
moved to HELIOS One, and followed Ignacio's dialogue tree: choose the
scientific response, state that you keep the peace, ask what more there is to
the plant, then choose “You think ARCHIMEDES is a weapon they built here.” INFO
`000E35FF` was naturally offered. Its result set
`VMS03.bDiscussedWeaponWithIgnacio` and called `SetStage vDialogueEDE 10`; the
viewer then emitted the quest message “Two EDE logs heard.” This confirms the
natural trigger reached the quest's second-log branch. The probe ended with
Ignacio's dialogue still open and did not capture the second log's INFO,
`iLogsPlayed == 2`, or voice. The playback remains unverified; this viewer
result does not show that the original game fails to play it. In a follow-up,
Escape was scheduled after the result and the run continued to 500 seconds, but
the screenshot still showed Ignacio's dialogue choices. No second-log INFO,
`iLogsPlayed == 2`, or voice appeared. Because the dialogue did not close, this
does not isolate whether the package would play after a successful close.
Earlier 56-second probes were too short to satisfy the five-in-game-day
condition on the quest data's second-log path.

## Validation status

FalloutNV.esm data confirms the repair and dialogue result scripts described
above. A private Linux viewer run on 2026-10-26 recorded the repair,
recruitment, Gibson INFO `000841E7`, the stage-10 result script, and first-log
INFO `00157F17`; the first-log route succeeded without a quest-stage or quest-
variable console command. The separate Ignacio probe recorded the naturally
offered INFO `000E35FF`, its result script, and the “Two EDE logs heard” quest
message after 132 in-game hours. It did not record log two playback or
`iLogsPlayed == 2`. Log one replayed after moving cells. Both runs used
XWayland inside KDE Wayland: this branch's Winit build enables X11 but not
Wayland. The revised PowerShell wrapper itself has not been run on Fedora
(`pwsh` is unavailable).

The revised route's acceptance checks require the repair activation, quest
start notice, hire INFO, Gibson INFO, stage-10 result script, and first log
INFO to appear in the viewer log. Passing those checks will show the
implemented route ran; it will not establish that later stages are complete
or match the original.

On 2026-10-26, the current PR #87 release build passed the equivalent Linux
viewer route through the first log: all acceptance markers appeared and no
panic occurred. The separate 132-hour probe reached Ignacio INFO `000E35FF`
and the quest's “Two EDE logs heard” message, but left the conversation open
before log-two playback could be verified. The private screenshot was written.
These runs used XWayland (`DISPLAY=:0`) in a KDE Wayland session because this
branch's Winit build has no Wayland backend; the PowerShell wrapper itself
remains untested because `pwsh` is unavailable.

## Remaining quest coverage

- The natural Ignacio trigger is verified: after the five-day gate, INFO
  `000E35FF` called `SetStage vDialogueEDE 10` and the viewer emitted “Two EDE
  logs heard.” The second log's INFO, voice and `iLogsPlayed == 2` were not
  observed, so playback is still unverified. The probe left Ignacio's
  conversation open even after a timed Escape. Stop route coverage here until
  dialogue closure and the second package can be observed without forcing
  quest state.
- The first log replayed after the cell transition. The temporary dialogue-
  package lifecycle still needs comparison with the original executable.
- The radio message branch, package cleanup after a finished log, holotape
  pickup, Hidden Valley discovery, Followers handover, return-to-Primm
  `MenuMode` step, completion package, and 100 XP are not covered here.
- The quest script (`00157F1F`) awards 100 XP after the quest is completed and
  before stage 100, guarded by `bCompleteOnce`; this route has not reached that
  script condition or verified the reward meter in the viewer.
- The previous 257-second route recorded `XP +100`, but it forced quest state,
  stages, calendar advances, package cleanup, item delivery, and travel. That
  run is historical evidence only and does not prove natural quest completion.
- The XP meter was not visually verified, audio was not checked, and no
  side-by-side original-game comparison was made.

## Implementation provenance retained for later route work

**Pip-Boy `MenuMode`.** In FalloutNV.exe 1.4.0.525, `MenuModeFunction`
(`005c4240`) calls `GetMenuModeConditionFunction` (`0059c380`) to evaluate
whether any menu or the requested class is open. The check is a condition,
not an open/close event. `Pipboy::top_class` contributes the open menu class
to `run_scripts` on each viewer script tick; duplicate classes are dispatched
once. The current partial route does not reach the Pip-Boy return step.

**Map discovery.** The game's proximity loop at `00779070` checks the player's
3D distance against a marker radius, then sets visible and travel bits through
`0044de40` and `0044de80`. `ShowMap` at `005c8620` can set visibility alone or
both bits; `GetMapMarkerVisible` at `005daac0` reads the 0/1/2 state through
`005a51e0`. The Hidden Valley marker (`000CDA74`) has `XRDS = 10,000`. In the
viewer, `world::map::discover` records a discovered marker, which the map
state exposes as visible and travelable. The partial ED-E route does not
exercise this path.
