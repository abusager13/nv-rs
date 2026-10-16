# Local co-op experiment

This is an experiment for a 2–4 player Fallout: New Vegas co-op
mode in nv-rs. Its design and packet demo live under `experiments/coop-sync`;
the first host/client movement integration lives in the Bevy viewer. The
initial prototype was pushed to `abusager13/nv-rs` on branch
`codex/coop-experiment`; current follow-up work is on the local-only branch
`codex/coop-experiment-local`. There is no pull request.

The target is a host-authoritative asynchronous shared world implemented in
nv-rs's Rust/Bevy simulation. Each player has their own character, inventory,
faction reputation, and faction quest progress. The host owns the shared
Mojave: actor and object changes, unique deaths, physics platforms, encounter
spawns, and any other world-wide consequences. See [ARCHITECTURE.md](ARCHITECTURE.md)
for the design and implementation gaps.

## Current prototype

`src/lib.rs` provides the versioned UDP packet codec used by both the standalone
demo and viewer. In viewer co-op mode, one local player can host and up to three
clients can join the same loaded cell. The host advances guest movement with
the viewer's character collision controller and sends 20 Hz pose snapshots;
clients predict their own movement and smooth toward the host's pose. Remote
players appear as colored capsules. The host journals actor death/revival,
reference destruction/restoration, and loaded actors' nonfatal health. Each
snapshot repeats up to 80 recent revisioned events. On join or a revision gap,
the client requests a keyframe containing current death/destruction flags and
loaded-actor health; the host repeats bounded chunks until the client
acknowledges the assembled baseline, then event replay continues from it.

Each client also sends its own quest stages, completed-stage flags, objective
completion, quest running/completed/failed flags, and faction fame/infamy to
the host. The host keeps that report on the peer that sent it and acknowledges
its highest contiguous event; it does not send one player's campaign state to
other players. Missing revisions remain pending for retransmission. The host
checks quest, stage, objective, and reputation IDs against loaded game data,
but does not validate causal quest transitions. This is a progress mirror,
not a save format. Updates are sent in batches of up to 64 and retained until
the host's snapshot acknowledges them; the client's pending queue holds up to
4096 events.

Each client may report an observed death/destruction transition in its
revisioned update stream. The host acknowledges it only if the same transition
is already present in the host's state; it never applies a guest claim. The
host's own journal remains the source sent to all peers. This prevents a guest
from declaring a shared actor dead or a reference destroyed, but guest actions
that have not been moved to host-side simulation will not propagate. The
bounded client queue holds up to 4096 events and retains them until
acknowledged.

This is a same-cell prototype, not the complete co-op design above. A viewer
process still has one local player; the host uses its current game state and
movement settings for guest locomotion. In co-op mode a local menu or dialogue
overlay stops only that player's movement; V.A.T.S. leaves virtual time at
normal speed. The host checks a selected V.A.T.S. target for life state, range,
line of sight, and both of the reported camera's view angles before the client
can queue an attack. Movement packets carry camera pitch and viewport aspect;
the host bounds that aspect and derives the matching view frustum from the
game's FOV.
Each shot waits for fresh target eligibility. The host validates the attack
kind, selected body part, weapon data, reload surcharge, and AP price, then
reserves AP once for the queued attack. Results are idempotent on retransmit.
Protocol version 13 carries pitch and viewport aspect for aim validation, as
well as a shot index and host hit/miss result for each shot. The attack packet
contains no hit probability: the host calculates it from the selected weapon,
body part, target distance, host-owned game state, and a host ray from the
guest's validated position to the target's center. This visibility estimate
does not yet account for body-part occlusion. Protocol 16 adds an acknowledged
guest combat profile with level, actor values, and perk ranks. The host applies
that profile temporarily while calculating AP limits, regeneration, V.A.T.S.
costs and hit chances, and ordinary attack damage. The host rolls the result
and applies V.A.T.S. damage through nv-rs's combat runner against shared host
state. Ordinary guest hitscan shots
and melee swings also go to the host, which resolves them from the latest
reported guest position and camera direction against host cell geometry. The
host applies actor damage and the existing actor-health replay carries it to
guests. Protocol 17 death events from host-resolved V.A.T.S., hitscan, melee,
bolt, and thrown-weapon kills carry the assigned player ID. Flying bolts and
thrown explosives retain the guest's combat profile from launch through their
delayed impact. The underlying game death event, kill rewards, karma,
statistics, and ragdoll nudge still treat the attacker as the host's
`PLAYER_REF`. Flying bolts and thrown weapons launch and impact on the host;
guests consume a thrown item from their own local inventory only after host
acceptance. Protocol 15 sends an acknowledged combat-inventory snapshot with
weapon and compatible ammo counts. Attacks name the inventory revision and
selected ammo; the host checks ownership, uses that ammo in damage resolution,
and decrements the mirrored count after an accepted shot. The host also
decrements its guest weapon count after an accepted throw; the guest consumes
the thrown item locally then. Other inventory remains local. Damage
calculations use the guest profile through the host's single `PLAYER_REF`;
profile-changing script side effects are still not retained per guest.
Attacks wait until
both the inventory and profile revisions
are acknowledged. Results acknowledge that
the request was processed, while the health stream communicates world
outcomes. Actor-health events and keyframes reconcile nonfatal damage on
guests. Aim direction and viewport aspect remain client-reported, so target
geometry validation does not authenticate the camera. Kill rewards, attack
cancellation recovery, full inventory validation and transfers, moving
platforms, and AI state are not yet synchronized. Each client sends quest and
faction progress reports to the host.
Quest stages, stage completion, objectives, and status are applied to a shared
host ledger and replayed to every client; missing revisions remain pending
until acknowledged. The host checks quest, stage, objective, and reputation
IDs against loaded game data. For a shared stage report, the host runs
`Runner::set_stage` against its own state, including stage conditions and the
result script; clients run the accepted stage when replaying it. Stage-done,
objective, and status reports reach the shared ledger only if the host's state
already matches after its own processing. The guest still initiates stage
reports. Protocol 19 sends sequenced dialogue-choice requests that include the
client's current offer and selected topic/INFO pair. The client retransmits its
pending choice until the host acknowledges it; the host caches the last result
to make retransmits idempotent, validates the loaded records and current
speaker visibility, then mirrors shared begin/end scripts. INFO records
attached to configured private quests stay client-local. Campaign events also
carry quest-script variables by their loaded script index. The host checks the
quest and variable index; shared variables are accepted only when the host's
simulation already has the reported value. Private quest variables remain in
the per-peer ledger and are installed while that player's scripts run. The client
waits for its outstanding campaign deltas to be acknowledged before sending a
dialogue choice. Menus remain client-rendered, while the host reconstructs
eligible choices from loaded records and the guest's synchronized quest,
dialogue, reputation, gender, inventory, and combat-profile state before
accepting the selected topic/INFO pair. The choice request includes its
source INFO when the menu came from an explicit `TCLT` list. The host still
does not isolate every
`PLAYER_REF` side effect. Protocol 24 carries the guest's gender and item
counts, including an explicit marker for the complete initial inventory
snapshot, so the host can evaluate common gender and possession conditions
against that player's state. Other unsupported player-specific conditions may
still differ between host and guest. Protocol 24 carries
learned dialogue topics, said INFO records, and talked-to speaker IDs through
the acknowledged campaign stream, so the host can install a guest's dialogue
history while resolving that guest's scripts. Protocol 24 also replicates
changed `GLOB` values. Globals are shared by default; the host can
scope selected values per player with `--coop-private-globals
GLOBAL[,GLOBAL...]`, using GLOB editor IDs or `0x` form IDs. A peer's private
globals are held in that peer's campaign ledger and installed while its
dialogue scripts run. A guest's shared-global report is accepted only when
the host's own simulation already has the same value. The host overlays the
peer's configured private quest stages, objectives, status, globals, and
reputation while scripts run, captures private changes back to that ledger,
and restores the host's values. Protocol 24 also installs the guest's
miscellaneous-stat baseline, keeps its stats available to guest-context
scripts, and replays host-authored XP, karma, and stat changes only to that
player. Guest attacks and mirrored dialogue updates wait for the initial
campaign, inventory, and profile updates to be acknowledged. The host holds
the updated profile for their next attack. Reputation
is always private. Quest progress is shared
by default; the host can mark faction quests private with
`--coop-private-quests QUEST[,QUEST...]`, using QUST editor IDs or `0x` form
IDs (`nvinspect <Data folder> list QUST` lists the editor IDs). For example,
the host can add `--coop-private-quests VMQ02,VMQHouse1`; guests do not need to
repeat the list. This explicit list lets the group keep faction paths separate
without guessing scope from quest names. Death replication changes the
host-owned dead flag; it does not include death animation or ragdoll state.
World keyframes are limited to 4096 chunks of at most 130 variable-size entity
entries each; campaign updates are
retransmitted until acknowledged unless more than 4096 events accumulate in
the pending queue. The campaign report omits other save data. Dialogue menus
remain local. The host accepts a dialogue
snapshot only when the same view-caster used for the local crosshair, run with
the guest's latest reported camera direction and the host's cell geometry,
picks a loaded, living speaker. It forwards the nearest speaker's `INFO`/`DIAL`
form IDs and response index in a read-only listener overlay to players within
512 game units. Each client resolves names and text from its own game records;
snapshots expire if the speaker stops sending updates. Camera direction and
the offered choice list remain client-reported. Protocol 19 carries the
selected topic/INFO pair with its offer in a reliable, acknowledged request;
the host checks the speaker and records, mirrors the selected line's begin
script, and runs an active line's end script on transition. Record-listed
follow-ups can also be mirrored from dialogue snapshots. The host ends a
tracked line if its dialogue report expires. INFO records attached
to configured private quests are skipped on the host. It installs the peer's
private campaign facts, globals, reputation, and dialogue history while
scripts run, then restores the host's values. Globals not listed as private
remain shared.
Challenge progress and other script effects through `PLAYER_REF` still need
isolation. The speaker's
local world simulation continues while the menu is open. Players
must start in the same cell with identical loaded plugin contents. The
handshake fingerprints plugin names and complete plugin files, but not
archives or loose assets. Cell transitions and Internet play are not
supported. UDP messages are unauthenticated. Combat profiles are checked for
finite values and loaded perk records, but the host cannot independently
verify a guest's reported character values.

## Run two local viewer instances

From the repository root, start one host:

```sh
cargo run --manifest-path viewer/Cargo.toml -- \
  "/path/to/Fallout New Vegas/Data" GSDocMitchellHouse \
  --coop-host 0.0.0.0:39000
```

In another terminal, start a guest in the same cell:

```sh
cargo run --manifest-path viewer/Cargo.toml -- \
  "/path/to/Fallout New Vegas/Data" GSDocMitchellHouse \
  --coop-join 127.0.0.1:39000
```

For a second computer on the same network, use the host computer's local
address in `--coop-join` and allow UDP port 39000 through its firewall. On
macOS with FalloutNV.exe 1.4.0.525, a protocol version 7 host listened on
127.0.0.1:39124; a guest loaded the same house cell and joined as player 1.
During a scheduled four-second left movement input, the host logged the input
and simulated the guest at (2352.1, 1646.0, 7360.9). This confirms the local
guest-input → host-movement path in two instances on one Mac. A later run on
port 39131 exercised two guest V.A.T.S. attacks on Doc Mitchell. The host
approved attack 0 at 63 AP and attack 1 at 58.7 AP after regeneration; the
guest reconciled to about 59 AP. This caught a reused attack ID after reopening
V.A.T.S.; request IDs now remain unique across menu sessions. This confirms
local host AP approval and guest reconciliation, but not remote-network play,
correction under latency, or current protocol behavior. Host damage,
nonfatal-health replication, keyframe recovery, dialogue listen-in,
guest-update rejection, and ordinary host-resolved hitscan and melee attacks
still need live validation. The combat-inventory mirror and protocol 16 guest
combat-profile handshake also need live checks. Game audio was left enabled. A first launch exposed a macOS
Metal surface panic; the optional mailbox probe is now disabled on macOS so
Bevy keeps its default present mode.

A later protocol 10 run loaded the house in two local viewers and confirmed
that the guest joined. On leaving for the exterior, Metal rejected a render
pipeline with 17 fragment samplers (limit: 16). On macOS, the distant-land
material now uses a standalone Bevy material to remove its unused inherited
PBR bindings; other platforms keep the existing extended PBR material.
`cargo check` and a viewer build pass. The exterior fix has not been verified
at runtime because the user asked not to run the viewer. The viewer processes
are closed.

## Run the packet demo

From the repository root, start the host:

```sh
cargo run --manifest-path experiments/coop-sync/Cargo.toml -- host
```

In another terminal, start a client that moves along X for five seconds:

```sh
cargo run --manifest-path experiments/coop-sync/Cargo.toml -- client 127.0.0.1:39000 1 0 5
```

Start up to four clients with different directions to see the host track them.
This packet demo is for local networking only; it has no authentication,
encryption, state events, or Internet discovery.
