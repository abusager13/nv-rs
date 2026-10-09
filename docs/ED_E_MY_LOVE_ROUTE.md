# Primm route: ED-E My Love (`vDialogueEDE`)

Issue #13, ED-E My Love. Quest `vDialogueEDE` **001572E8**, quest script
`vDialogueEDESCRIPT` **00157F1F**, ED-E's own script `EDEScript` **00151D03**.
Installed data, viewer release build. Logs and screenshots are private and
never committed.

**Nothing here has been compared with the original game.** "Reached" means the
viewer's own scripts, packages and dialogue did it; steps marked *forced* were
done with console lines standing in for something the viewer cannot do yet
(listed under blockers).

## The quest, from the data

The quest is start-game-enabled and allows repeated stages (`DATA` 0x00004B19).
Its variables, by the slot number a `GetQuestVariable` names (`SLSD`/`SCVR` in
SCPT 00157F1F): `iLogsPlayed` 1, `fLastLogDay` 2, `iPlayRadio` 4, `iEdeRadio` 5,
`iEDEOut` 6, `iEDEDays` 7, `iHVUpgrade` 8, `iFolUpgrade` 9, `iEDEDaysPassed` 10,
`iAprilDead` 11, `iCounter` 12, `bEDEExamined` 13, `bGibsonOnce` 14, `iDoOnce`
15, `bCompleteOnce` 17.

| Stage | Flag | Set by | Result |
| --- | --- | --- | --- |
| 10 | — | any of the 40 keyword lines that `SetStage vDialogueEDE 10` (Old Lady Gibson's greeting, INFO 000841E7, is the Primm one) | its first passing entry runs the log dispatcher: `fLastLogDay` = the day, objective 1; with `iLogsPlayed == 0`, ED-E gets `EDEDialoguePackage02` (0015882D, topic `EDELog1`) and objective 5 |
| 10 | — | a second `SetStage vDialogueEDE 10` while `iLogsPlayed == 1` | `EDEDialoguePackage03` (0015882E, topic `EDELog2`) and stage 20 |
| 20 | — | the dispatcher above | "both logs heard"; no conditions of its own |
| 100 | **0x02 FAILS_QUEST** | `GetQuestCompleted == 0` and both `VFSEDEScientistRef.GetDead` and `HVKnightEDERef.GetDead` | the quest fails (its failure notice) |

The logs themselves: INFO 00157F17 (`EDELog1`, sets `iLogsPlayed` to 1) and INFO
00158829 (`EDELog2`, sets it to 2), each with `EDEContinue…` and `EDEEndLog`
follow-ups.

The rest of the quest script's GameMode, in order:

* the radio branch: with `iEDEDaysPassed >= 2` and `iLogsPlayed >= 2` and
  `iPlayRadio == 0` and ED-E hired, it picks `iEDERadio` (1, 2 or 4 by whether
  the Hidden Valley knight is dead and whether `VMS55` has been reached) and
  asks ED-E to evaluate its package; a second step waits for the Hidden Valley
  marker to be travel-visible and objective 13 or 15;
* the handover: `iEDEOut` 1 → 2 (records `iEDEDays` = the day ED-E was taken) →
  after `GameDaysPassed - iEDEDays >= 3` → 3; the Menumode block on 3 moves the
  upgraded `EDE2Ref`/`EDE3Ref` to `EDEHomeMarker` and sets 4; on 4, while the
  player is **outside** `PrimmNashResidence`, GameMode enables the upgraded
  ED-E, shows the return notice and completes objective 60;
* the upgrade swap (for whichever of `iFolUpgrade`/`iHVUpgrade` is 1): ED-E's
  items move to the new form, the old one is disabled and moved away, the perks
  and teammate flags are cleared, the follower count lowered, `iEDEOut` set to 1
  again;
* **completion**: `bCompleteOnce == 0` and `GetQuestCompleted == 1` and
  `GetStage < 100` → `RewardXP 100` once. This is the success reward, and it is
  gated on the completed flag being set *without* stage 100.

The completed flag is set by ED-E's own package: **PACK 0016041A
`EDEQuestCompleteDialogue`**, type 15 (Dialogue), the **first** package in
`EDE2Ref` (ACRE 001732D0, base CREA 001694E2) and `EDE3Ref` (ACRE 001732CF, base
CREA 001694E0) base lists. Its condition is `GetQuestVariable(vDialogueEDE, 6)
== 4`, i.e. `iEDEOut == 4`; its End action's script is `set vDialogueEDE.iEDEOut
to 5` then `completequest vDialogueEDE`. Its own dialogue procedure takes it to
`EDEHomeMarker` (REFR 000A23D1) and talks to the player while the player is in
the second location, which the package names as the **cell** `PrimmNashResidence`
(PLD2 kind 1, form 000D70E2).

## How far the route got

The acceptance route (`ede` in `scripts/acceptance.ps1`) replays the completion
segment. The state the quest's own scripts would have reached is set with console
lines, then ED-E's own package does the rest — no `AddScriptPackage`, no forced
conversation:

```
nv-viewer.exe <Data> PrimmNashResidence
  --run "StartQuest vDialogueEDE" --run "set vDialogueEDE.iEDEOut to 4"
  --run "EDE2Ref.Enable" --run "EDE2Ref.MoveTo EDEHomeMarker"
  --say "Log Off" --wait 60
  --answer-boxes --box-answers 2 --screenshot <private>
```

| Step | Evidence |
| --- | --- |
| ED-E's completion package runs | the log records `EDE2Ref: package EDEQuestCompleteDialogue End action` |
| the quest completes | `Quest completed: ED-E My Love` |
| the reward branch fires | `XP +100` |
| the Pip-Boy agrees | the STATS page shows XP 100/200 |

Verified in the Linux release viewer (Vulkan backend). The earlier run that
killed April and the Hidden Valley knight produced the *failure* notices, not
this; those commands are gone.

## Fixes (one commit each)

### The dialogue package's second location in a cell

`EDEQuestCompleteDialogue`'s second location (`PLD2`) is kind 1, "in a cell",
naming `PrimmNashResidence`. `at_second_location` (`crates/world/src/ai.rs`)
returned `false` for anything but kind 0, so `dialogue_step` answered
`DialogueStep::Wait` on every update: the package could never reach its Talk
step, so its End action (the `completequest`) never ran and the quest never
completed — the handover and objective 60 were unaffected because they are
quest-script GameMode, not this package. The location kinds are the loader's
(`0067f060`; the cell comparison as `00676390`). The fix compares the actor's
current cell with the package's cell for kind 1. Test: a generated type-15
dialogue package whose second location is kind 1 (`crates/testdata/src/ai.rs`)
with a regression in `crates/world/tests/ai.rs` (matching cell → `Talk`,
different cell → not at the location).

## Blockers left (not fixed here)

1. **The manual Pip-Boy `MenuMode` dispatch.** The quest's Menumode block is
   what sets `iEDEOut` 3 → 4, and nothing in the viewer's manual Pip-Boy path
   calls `Runner::menu_mode`, so on a played route (no console) that step never
   runs and the return/objective-60/completion chain never starts. The route
   above forces `set vDialogueEDE.iEDEOut to 4` instead. The traced shape of the
   fix: forward the shown Pip-Boy menu's class to `Runner::menu_mode` while it
   is up, and mirror the engine's `MenuMode n` predicate (`0059c380`, through
   its handler `005c4240`) — no argument or `0` any menu, `1` a Pip-Boy menu
   (1002, 1003, 1023, 1035, 1061), else that menu id.
2. **The natural second keyword.** No non-console trigger for log two has been
   played; every run forced the second `SetStage vDialogueEDE 10`.
3. **The map-marker travel semantics.** `GetMapMarkerVisible` should answer 0
   hidden, 1 visible, 2 also travel-able (`005daac0` → `005a51e0`); the radio
   branch's `== 2` test depends on it.
4. **Stage 10's entry gate.** The `iCounter == 5` condition and the
   `GetScriptVariable(001732D1, 0x10)` condition (its form and script
   unresolved) were not measured at dispatch; which entry runs is not explained
   by the record alone.

## Not compared with the original game

Nothing in this route has been watched in the original game. The completion
segment above is the viewer's own package, script and reward, with the four
console lines named; the played route up to objective 60 (prerequisite steps
2–4) still needs the MenuMode dispatch before it can run without a console.
