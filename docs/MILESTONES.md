# nv-rs milestones

Updated 2026-10-15. The active plan and how each step is judged done. Keep
this file short: per-session detail goes in docs/HANDOFF.md, per-function
status in [LEDGER.md](LEDGER.md), and the full list of systems in
[SYSTEMS.md](SYSTEMS.md). The log up to 2026-10-14 is in
[history/milestones-2026-10-14.md](history/milestones-2026-10-14.md).

## The plan

nv-rs ports FalloutNV.exe 1.4.0.525 system by system
([ENGINE_PORT_PLAN.md](ENGINE_PORT_PLAN.md)). Translated code in the
`engine` crate replaces the hand-written systems one at a time. Each
switch-over ships as a play build.

A milestone is done when all three of these hold:
1. **Translated.** Every function in the system's units is translated, or
   partial only for compiler-only gaps (exception frames, stack cookies).
2. **Switched on.** The viewer runs the translation in play instead of the
   hand-written system, with a fallback where one is still needed.
3. **Verified.** The acceptance routes pass, and the behaviour matches the
   original game: a recording, a side-by-side comparison, or the
   original's own outputs.

## Engine port

| Milestone | What it covers | Status |
| --- | --- | --- |
| P0: Map and ledger | Every exe function has a unit, a subsystem and a status | Done |
| P1: Frame skeleton | The game's per-frame order drives the app's systems ([FRAME_SKELETON.md](FRAME_SKELETON.md)) | Done |
| P2: Engine in play | The exe check at start-up, the start-up setup (static initializers), and the script interpreter live for quests | Done (Build 42). Quests load through the game's own loader; stage items still need seven small functions (see HANDOFF) |
| A1: Movement and physics | Character controller, player and actor movement, camera collision, the Havok SDK and Bethesda's Havok layer, ragdolls | **Active.** The character controller and player mover are done; the Havok SDK (about 5,100 functions) has not started |
| A2: Animation and faces | NiAnimation, animation choice and blending, FaceGen and lip sync | Not started |
| A3: Audio | BSAudio, the radio and music; game threads stepped behind the thread seam | About two-thirds translated; the seam is not built |
| B: Graphics | Bevy upgrade first, then the scene graph, shaders and lighting, sky and weather, terrain and trees, effects | Not started |
| C: Gameplay | Actor values and combat, AI and pathfinding, quests and dialogue, menus and V.A.T.S., magic, progression, crafting, minigames, world streaming | Mostly translated; switch-overs not started |
| D: The rest | Original saves, shut-down, video, settings, real threads, the remaining unplaced functions | Not started |

Approximate timing (maintainer's goal): A and B in about a month, the whole
game in about two months, depending on the weekly budget.

## Product goals

These were the original milestones. The engine port now delivers them.

| Goal | Gate |
| --- | --- |
| Opening to Goodsprings | The opening, Doc Mitchell's character creation and Goodsprings play through on the original's logic; save and reload keep all state |
| Core loop | Sunny's tutorial and Ghost Town Gunfight on translated combat, AI, dialogue and progression |
| Campaign | The base game completes; old saves load |
| DLC and mods | The four DLCs, plugins and archives behave as in the original; script-extender (xNVSE, JIP) compatibility as a later layer |
| Stability | Long play without crashes or lost state, with measured frame times |
| VR | Shared simulation with independent aim and views, tested on a headset ([VR.md](VR.md)) |

## Rules that hold throughout

- Nothing is invented or tuned by eye. Behaviour comes from the exe, its
  data and recordings of the original ([METHODOLOGY.md](METHODOLOGY.md)).
- The original behaviour stays available. Any later bug fixes go in a
  switchable fixes layer.
- Completion needs a reproducible acceptance run, not a count of functions
  translated.

## Local co-op experiment

The initial prototype remains on the previously pushed fork branch
`codex/coop-experiment`. Current follow-up changes are on the local-only branch
`codex/coop-experiment-local`; there is no PR.
The viewer checks V.A.T.S. targets and AP, applies host-side V.A.T.S. hits, and
replicates actor health. Protocol 11 removes the guest-reported hit probability;
the host computes it from loaded weapon/body-part data, target distance, host
line of sight, and its own player stats. Protocol 11 also carries bounded
viewport aspect and camera pitch so host target checks cover both view angles.
Protocol 12 sends dialogue `INFO`/`DIAL` references and replays shared quest
stage, objective, completion, and status changes from the host until clients
acknowledge them. Dialogue listeners resolve text from local game records; the
host checks the record references and uses its view-caster against host cell
geometry and the guest's latest camera direction. Reports expire after 750 ms.
Protocol 13 routes guest hitscan gunfire and melee swings through the host.
The host uses its latest received guest pose and aim, resolves hits, flying
bolts, and thrown weapon impacts in host cell geometry, applies damage, and
replicates actor health. Guests consume their own thrown item after the host
accepts its launch. Protocol 15 carries an acknowledged mirror of combat
weapons and their compatible ammunition. Attack requests name an inventory
revision and ammo form; the host checks ownership/count, uses that ammo in the
combat runner, and reduces the guest mirror after accepted shots or throws.
The guest consumes ordinary ammo locally before sending its request and
consumes thrown items after acceptance. Protocol 16 adds an acknowledged
profile of level, actor values, and perk ranks. The host temporarily applies it
to combat cost and damage calculations. Protocol 17 carries the assigned guest
ID in shared death events from host-resolved V.A.T.S., hitscan, melee, flying
bolt, and thrown-weapon kills. Bolts and thrown explosives retain the guest's
combat profile from launch through delayed impact. The game's kill rewards,
karma, statistics, ragdoll nudge, and profile-changing script effects still
use the shared `PLAYER_REF`.
Guests choose locally. Protocol 19 sends each selected topic/INFO pair with its
client-reported offer and retransmits the request until acknowledged; the host
caches the result to prevent duplicate script execution, validates the loaded
records and current speaker, then mirrors shared begin/end scripts. It skips
INFO records attached to quests marked private. Protocol 24 sends the source
INFO for explicit choice lists and recomputes whether the selected response
passes against the guest's synchronized quest, dialogue, reputation, and
combat-profile state. The client still renders the menu locally. Protocol 24
also carries guest gender and item counts, with an explicit marker for the
complete initial inventory snapshot; the host waits for it before accepting
dialogue choices. Other unsupported player-specific conditions may still
differ. Campaign events carry quest-script locals by index; shared values are
accepted only if the host has already simulated
them, while configured private quest variables stay in each peer's ledger.
Protocol 24 carries learned dialogue topics, said INFO records, and talked-to
speaker IDs in the acknowledged campaign stream, so the host can install each
guest's history while resolving their dialogue scripts. Protocol 24 also
carries changed `GLOB` values. Globals are shared by default;
the host can scope selected globals per player with
`--coop-private-globals GLOBAL[,GLOBAL...]`. Private values live in the
per-peer ledger and are installed while that peer's dialogue scripts run.
Shared-global reports are accepted only when the host simulation already
matches.
During a shared dialogue script, the host
loads the peer's configured private quest stages, objectives, status, globals,
and reputation from its per-peer campaign ledger, captures private changes
there, and restores the host's state. Dialogue history is carried per player.
Reputation stays private, and the host can mark
faction quest IDs private with `--coop-private-quests`. For shared quest
updates, the host runs reported stages through `Runner::set_stage`; it
propagates stage-done, objective, and status changes only when its own state
already matches them after that processing. Stage reports themselves still
originate from the guest. The host overlays the guest's gender and inventory
while validating choices and running their dialogue scripts, then restores
the host player's state. Protocol 24 sends each guest's misc-stat baseline,
isolates stat reads and writes in guest context, and replicates targeted XP,
karma, and kill-stat changes. The host retains updated guest values for later
combat. Challenge progress and other script effects that target the shared
`PLAYER_REF` remain unresolved.
In co-op, the host now advances NPC AI while its own dialogue or menu overlay
is open and runs NPC detection near connected guests; single-player pause
behavior is unchanged. NPCs can still target only the host player, so
guest-targeted combat and host-authoritative damage to guests remain
outstanding.
Guest-specific conditions and other script side effects still need validation.
The combat profile also needs live multiplayer validation.
`cargo check` passes for the viewer and packet demo after protocol 24. Earlier
two-viewer macOS runs confirmed guest joining, host-simulated movement, and V.A.T.S. AP
reconciliation; protocols 15–24 and the per-peer dialogue campaign context
have only been compile-checked. Leaving Doc
Mitchell's house previously crashed while creating an outdoor render pipeline:
Metal reported 17 fragment samplers, over its limit of 16. On macOS only, the
distant-land material now uses its own standalone material instead of
inheriting `StandardMaterial` bindings; other platforms retain the existing
extended PBR material. A direct WastelandNV start rendered on macOS without
the sampler-limit error, but the actual house-exit transition has not been
reproduced and Windows comparison remains outstanding. The viewer has now
been upgraded from Bevy 0.16 to 0.20; `cargo check` passes on macOS. The
pause menu's Display page now has a MetalFX spatial-upscaling toggle on Apple
Silicon macOS when the framework is available. It uses a local adaptation of
`bevy_metalfx` 0.4.2 for Bevy 0.20 / wgpu 30; Off is the default and On renders
at 75% resolution. Only primary-window 3D views receive the resolution
override, and skipped intermediate views bypass the MetalFX output pass.
On 2026-10-15, the user asked for runtime testing on macOS. The release viewer
build passes and has been run against FalloutNV.exe 1.4.0.525 and the user's
Data folder. The house scene and pause menu rendered without the reported pink
frame. In a separate validation build with both the scaler and game setting
temporarily enabled, the house scene and HUD rendered at normal size; the
temporary default was reverted and Off remains the checked-in default. The
MetalFX node logged one initial "pipeline not ready yet" warning, then rendered
without a validation error or crash. A direct exterior start also loaded, but
the attempted scripted door interaction did not trigger a house-exit
transition. The scripted menu clicks reached the settings categories but did
not verify the Display page or the toggle interaction. No PR has been opened.
Apple's MetalFX scaler is explicitly set to Linear because the viewer supplies
linear RGBA16Float, but the matched On capture remains darker than Off and
softens cloud detail. Cloud meshes sit on the sky dome and fade toward the
horizon. A scripted 5-degree downward start produced a black frame in Off, and
the matching 25-degree Off capture was also black while an earlier
On capture showed only terrain. These inconsistent scripted results do not
validate the reported angle behavior. The Mac was locked during the attempted
interactive comparison, and no viewer is left running.
See the 2026-10-15 entry in [HANDOFF.md](HANDOFF.md) for evidence and remaining
checks. Next: verify the Display toggle and cloud angle through the UI when the
Mac is unlocked, and resolve color parity before recommending MetalFX On.
