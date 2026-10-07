# Primm deputy and sheriff route

Issue #13 asks for My Kind of Town to run in the viewer through the deputy
rescue and one sheriff outcome, with an acceptance route. The latest live
post-#26 viewer run completed that route; see the acceptance evidence below.

## Evidence

The reference executable is the user's FalloutNV.exe 1.4.0.525 (SHA-256
`61de3c742bbb6c586cd4441fa3b3533bd493e86ee06debd9d83ff4d817c6fafd`). Ghidra
12.1.4's project and exports are private under
`/tmp/nv-rs-issue13-research`; none are part of the repository. The native
`SetRestrained` command is `005d0920`, which calls `008ace50`. That routine
changes the actor's restrained life state and clears it when passed false; it
does not reveal a route gate. `EvaluatePackage` (`evp`) is native command
`005c95a0`; its handler resolves the reference, checks runtime guards, and has
a guarded branch to `008a6ce0`. In the viewer, the command queues an immediate
package check (`crates/world/src/scripting.rs`, `EvaluatePackage`; consumed by
`viewer/src/ai.rs`). This confirms the release script requests reevaluation,
but does not prove the selected package reaches its end.

The initial live runs below used base `33dee45`, before PR #26's NPC AI,
combat, dialogue, package, and pathing work. They preserve useful dialogue and
quest-record evidence, but are not acceptance evidence for current `main`
(`23a76c9`). The focused post-#26 retest is documented below.

Game-data records were read from the user's installed Data folder with
`nvinspect`:

- Nash's Primm INFO `0015A78D` sets
  `nVPrimmDeputyConv.DeputyHostage` to 1 and starts stage 20.
- Beagle's release INFO `000BACD5` sets stages 20 and 25, stops combat, and
  calls `SetRestrained 0`.
- Primm Slim's successful Science dialogue INFO `000EC078` sets
  `BeagleCaptured` to 4 and `PrimmSlimSheriffState` to 1, disables the dead
  sheriff references, sets stage 130, and awards 30 XP. The quest stage's own
  script awards 300 XP and completes all objectives.

On 2026-10-07, a live viewer run from `VikkiAndVance` used Nash's dialogue,
moved the player to `PrimmDeputyRef`, and selected Beagle's actual “I'll set
you free now” response. The log records the stage-20/25 scripts, completed
rescue objective, and the follow-up objective. The run used
`player.ModAV Health 5000` to survive nearby Powder Gangers. Private log:
`/tmp/nv-rs-issue13-research/rescue-durable.log`.

That run also records the boundary after release: Beagle's second script sets
`BeagleCaptured` to 1 and starts `PrimmDeputyRef.evp`; his next dialogue says
he is still extricating himself, then that he is busy until he reaches safety.
The player's stationary position during that exchange is expected. The log
does not establish that Beagle reaches safety or agrees to be sheriff, so the
unfinished link is his post-release travel and follow-up dialogue.

The installed-data package `PrimmDeputyLeaveBison` (`000DA117`, type 6) has
end/change actions that set `BeagleCaptured` and `BeagleFollow` to 2, clear his
waiting state, remove him from the player and captive factions, and set his
assistance value. The package targets marker `000CD9B1`. The live post-#26
run below confirms the package reaches its end action after traveling to the
casino.

The Slim ending was also exercised through its real dialogue and quest scripts,
but with `BeagleCaptured = 3` and stage 26 supplied as test setup, plus Science
30. INFO `000EC078` ran, stage 130 completed the quest objectives, and the
route awarded its 30-XP response reward and 300-XP stage reward. This confirms
the ending branch only; the setup skips the in-game steps and is not counted
as end-to-end evidence. Private log:
`/tmp/nv-rs-issue13-research/slim-resolution.log`.

## Current implementation and acceptance evidence

Current `main` already has `--run-at` timed test commands and ordered `--say`
topic selection for reproducible route runs. The corrected post-#26 Nash
sequence now selects “I have some questions about Primm,” then “What happened
to Primm?” and starts stage 20. The same live run reaches stage 25 through
Beagle's actual release and follow dialogue and completes the rescue objective.
`--dismiss-ok` closes modal messages with one OK button. These are verified on
the current branch; private logs and screenshots are under
`/tmp/nv-rs-issue13-research`.

The end-to-end route ran on the post-#26 branch with no quest stage or quest
variable set by console. Its final log is
`/tmp/nv-rs-issue13-research/primm-acceptance-final3.log`; the screenshot is
`/tmp/nv-rs-issue13-research/primm-acceptance-final3.png`. The Linux viewer
run confirms:

- Nash's INFO `0015A78D` starts stage 20. Beagle's release and follow INFOs
  run stages 20/25, complete the rescue objective, and display the new-sheriff
  objective.
- Beagle's `PrimmDeputyLeaveBison` package walks 5,663 units, enters
  `VikkiAndVance`, and runs its end action. The actor then uses his casino
  sandbox package.
- Nash's INFO `00162C19` displays the optional Slim objective. Slim offers
  the Science 30 response, and INFO `000EC078` succeeds. Its result script
  sets `BeagleCaptured` to 4 and `PrimmSlimSheriffState` to 1, disables the
  dead sheriff references, sets stage 130, and awards 30 XP. The stage
  completes both remaining objectives and awards 300 XP.
- The viewer reports “You reprogrammed Primm Slim to act as Sheriff of
  Primm,” both objective completions, `XP +300`, and `XP +30`. The XP opens
  the level-up menu as expected.

The acceptance route supplies only test setup and player interaction:
`player.ModAV Health 5000` and `player.SetAV Science 30`; `player.MoveTo`
Beagle, `PrimmDeputyExitMarker`, Beagle again, Nash (`000E2882`), and Slim
(`000E288C`); initial `--talk` and `StartConversation` on Beagle, Nash and
Slim; and `--say` to choose dialogue responses. No quest stage or quest
variable is forced. The route uses console `StartConversation` in place of
the player's look/activate interaction; that input method has not been
compared with the original game.

The acceptance script's final wait is 600 seconds so the last voice line and
quest stage script finish before the screenshot. The route was run directly
on Linux; `pwsh` is unavailable here, so the PowerShell wrapper itself could
not be executed on this host. This verifies the route behavior and expected
log output, but does not claim a side-by-side comparison with the original.
