# ED-E My Love: Primm start

Quest `vDialogueEDE` (`001572E8`), quest script `vDialogueEDESCRIPT`
(`00157F1F`), disabled-robot script `EDEScriptDisabled` (`001572E0`). This
route is **PARTIAL** and stays opt-in (`pwsh -File scripts/acceptance.ps1
-Routes ede -Data <Data>`). It covers the quest's Primm start through the first
audio log. A separate probe naturally reached the second-log trigger in
Ignacio's dialogue, but the viewer did not start log two or reach stage 20 in
the 220 seconds after that trigger. It has not been compared side by side with
Fallout: New Vegas. The revised PowerShell acceptance wrapper was not run on
Fedora because `pwsh` is unavailable; its first-log viewer command was run
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

A separate probe moved to HELIOS One and followed Ignacio's dialogue tree:
choose the scientific response, state that you keep the peace, ask what more
there is to the plant, then choose “You think ARCHIMEDES is a weapon they built
here.” INFO `000E35FF` was naturally offered; its result sets
`VMS03.bDiscussedWeaponWithIgnacio` and calls `SetStage vDialogueEDE 10`. The
probe closed the conversation and ran to 220 seconds total (about 56 seconds
after closing it), but recorded no second-log INFO, `EDEDialoguePackage03`, or
stage 20. This is a viewer result, not evidence that the original game fails
to play the log.

## Validation status

FalloutNV.esm data confirms the repair and dialogue result scripts described
above. A private Wayland viewer run on 2026-10-26 recorded the repair,
recruitment, Gibson INFO `000841E7`, the stage-10 result script, and first-log
INFO `00157F17`; the first-log route succeeded without a quest-stage or quest-
variable console command. The separate Ignacio probe recorded the naturally
offered INFO `000E35FF` and its stage-10 result script, but no second log or
stage 20. The probe also saw log one replay after moving cells. The revised
PowerShell wrapper itself has not been run on Fedora (`pwsh` is unavailable).

The revised route's acceptance checks require the repair activation, quest
start notice, hire INFO, Gibson INFO, stage-10 result script, and first log
INFO to appear in the viewer log. Passing those checks will show the
implemented route ran; it will not establish that later stages are complete
or match the original.

## Remaining quest coverage

- The natural Ignacio trigger is now identified as INFO `000E35FF`, but the
  viewer did not start the second log after its stage-10 result. Trace whether
  this is repeated-stage handling or package evaluation before changing code.
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
