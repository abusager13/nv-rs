# Co-op design and implementation boundary

This is the target for the local nv-rs experiment. The co-op simulation will
run in nv-rs's Rust/Bevy code and does not rely on networking support in
Bethesda's engine. This is a design proposal, not a description of features
already implemented.

## Authority and state ownership

The host runs the authoritative simulation and owns one shared, asynchronous
world. Bevy's ECS should represent each co-op player as a distinct player
entity with a stable session `PlayerId`; that identity must not be confused
with the game's single `PLAYER_REF` constant or a plugin `FormId`.

State has explicit scope:

- **Player-owned:** character stats, health, inventory, equipment, faction
  reputation, and faction quest progress. Players can pursue different
  factions and quests at the same time.
- **World-owned:** shared actor and object changes, unique deaths, moved or
  destroyed references, doors and platforms, and encounter state. A world
  change is committed once by the host and observed by everyone.
- **Derived:** UI offers, local aim, animation, and other view-only data.
  These are recalculated from the authoritative state and are not replicated
  as campaign truth.

An action carries its player identity, sequence, and the state revision it was
based on. The host validates it, applies the result to the correct scope, and
emits a versioned event. Unrelated player quests may progress concurrently;
conflicting writes to one shared actor, object, or world fact are ordered by
the host. A player can make a faction choice without changing another
player's personal reputation, while shared consequences such as a unique
NPC's death are visible to all players.

## Simulation and replication

The host advances world simulation continuously, including while a player has
a local dialogue or menu overlay open or is using V.A.T.S. Clients may predict
their own immediate movement and projectile visuals, but the host resolves
collisions, damage, AP costs, item transfers, and shared-world effects.
Authoritative updates reconcile client predictions.

In the viewer experiment, the host now keeps NPC AI moving while its dialogue
or menu overlay is open and runs NPC detection near connected guests. The
single-player pause remains unchanged, and clients continue to render
host-owned simulation. NPCs can still target only the host player;
guest-targeted combat and its authoritative damage path are outstanding.

Use sequenced, replaceable snapshots for transient movement and aim, and
reliable ordered events for quest changes, inventory transfers, actor/object
mutations, and cell transitions. Snapshots can send deltas from a named
baseline plus periodic full keyframes; a client that misses a baseline asks
for a fresh keyframe. This avoids requiring deterministic lockstep across
physics and script execution.

Each player's local simulation can have its own interaction session. Nearby
players may subscribe as dialogue listeners. The speaking player remains
vulnerable, and the host keeps the NPC and world active. Dialogue choices are
validated at commit time against the latest player and shared-world state.

## Feature decisions

- **V.A.T.S.:** real-time percentage-based aim assist. The client submits a
  target and queued attack; the host validates target eligibility, offered
  body part, weapon data, attack kind, and expected AP cost, then reserves AP
  once and rolls each shot before playback. The host applies hits through the
  nv-rs combat runner and replicates actor health. Protocol 16 sends an
  acknowledged guest profile of level, current/permanent actor values, and
  perk ranks; the host temporarily applies it while calculating hit chance,
  costs, AP limits, regeneration, and damage. Protocol 15 checks the selected
  weapon and ammunition against the guest's acknowledged combat-inventory
  mirror. The host uses the guest's selected ammo form for damage effects and
  reduces its mirrored count after an accepted shot. The visibility estimate
  does not account for body-part occlusion. Protocol 17 death events from
  host-resolved hitscan, melee, and V.A.T.S. carry the assigned guest ID.
  The translated game death event, kill rewards, karma, stats, and ragdoll
  nudge still use the single `PLAYER_REF`. Flying bolts and thrown explosives
  retain the guest ID and combat profile from launch through delayed impact.
  Attacker profile-changing script side effects are not retained per guest. Thrown
  weapons use host flight and impact
  simulation; the host decrements the guest weapon count after accepting a
  throw, and the guest consumes its matching local item. No world pause or
  client-declared hit result.
- **Moving platforms:** model platforms such as elevators and the monorail
  as authoritative parent physics frames. Players and objects riding one
  replicate a transform relative to that frame, with the host resolving
  collisions and carrying contacts.
- **Projectiles and item handoffs:** the host owns projectile identity,
  authoritative impacts, and item ownership. Clients can predict presentation;
  item transfer is a single versioned transaction so two players cannot own
  the same dropped weapon.
- **World deltas:** replicate changes to actors and references by stable IDs,
  with versioned baselines and keyframes. Cleanup such as corpse removal is
  a host event so every client applies it once.
- **Encounter pacing:** compute party pressure from each player's danger,
  health/resources, combat state, and distance from teammates. Allocate
  optional encounter budgets near eligible players. The director cannot
  override faction quest rules or spawn actors outside valid game-data
  locations.
- **Dialogue and menus:** a local overlay does not pause the host's clock or
  other players. Listen-in is a read-only subscription; the speaker alone
  makes personal quest choices unless a shared-world choice explicitly
  requires a party decision.

## Current code gaps

The standalone UDP demo has one host-assigned ID per socket and synthetic
positions. The viewer integration adds remote capsule entities and
host-authoritative guest movement for one loaded cell. The starting-cell
handshake includes a fingerprint of the ordered plugin names and complete
plugin contents, so peers with different form data cannot join; archives and
loose assets are not fingerprinted. Each running client has
its own local `GameState`, so its local scripts can make personal faction
choices. A client reports quest stages, stage completion, objective
completion, and quest status to its per-peer host ledger. Shared-scope quest
changes are applied to an ordered shared ledger and replayed to all clients;
the host can explicitly keep selected faction quests private. Valid record IDs
are checked. For a shared stage report, the host runs `Runner::set_stage` on
host state, and clients run the accepted stage during replay. Stage-done,
objective, and status reports propagate only when the host's state already
matches after its own stage processing. The stage report itself still
originates from the guest. Protocol 19 sends dialogue selections as sequenced
requests containing the client's offer and selected topic/INFO pair. The
client retransmits until acknowledged, and the host caches the last result so
a retransmit cannot run a script twice. The host validates loaded records and
the current speaker, then mirrors shared begin/end scripts; INFO records
attached to explicitly private quests remain client-local. Campaign events
also carry indexed quest-script variables. Shared variables are committed only
when the host's simulation already has the reported value; private-quest
variables are stored in the peer ledger. Protocol 24 carries learned dialogue
topics, said INFO records, and talked-to speaker IDs in the acknowledged
campaign stream, and replays the peer's history while host scripts run.
Protocol 24 also replicates changed `GLOB` values. Globals are shared by
default; the host can scope selected values per player with
`--coop-private-globals GLOBAL[,GLOBAL...]`. Shared-global reports are
committed only when the host simulation already matches; private values are
stored in the peer ledger. Dialogue menus remain local, but the host
recomputes whether a selected topic/INFO pair is currently available against
the guest's synchronized state and combat profile. Explicit `TCLT` menus carry
the source INFO so the host can rebuild the same choice set. Protocol 24 also
replicates guest gender and item counts, with a marker for the complete initial
inventory snapshot; the host waits for that snapshot before approving dialogue
choices. Other unsupported player-specific conditions may still differ. While
running a guest's shared dialogue script, the host
overlays that peer's configured private quest stages, objectives, status,
globals, and reputation from its per-peer campaign ledger, then stores private
changes back to that ledger and restores the host's values. Unconfigured
globals remain shared. Protocol 24 sends each guest's initial misc-stat
baseline to the host, resolves stat reads and writes in that guest's context,
and replays targeted XP, karma, and stat changes through the acknowledged
campaign stream. The host updates the guest's cached combat profile so later
attacks use the new actor values. Guest attacks and mirrored dialogue reports
wait for the initial campaign, inventory, and profile updates to be acknowledged.
Challenge progress still uses shared state.
Guest movement still uses
the host's player stats and movement settings. The viewer wires movement, V.A.T.S.,
dialogue, AI, and many script
conditions to one `PLAYER_REF`; `world::scripting::GameState` also mixes world
facts with player-specific facts. Bevy's ECS is a good fit for separate player
entities and player-keyed campaign resources, but nv-rs's systems still need a
player context and explicit state scope to use those entities consistently.

In co-op mode, frame scheduling keeps world and AI gates open while a local
menu or dialogue overlay is up, while the local player's movement gate and
interface still see the overlay. V.A.T.S. leaves virtual time at normal speed.
The host validates each queued V.A.T.S. attack's weapon data, kind, selected
body part, reload surcharge, and AP price, then reserves AP once. Protocol 11
numbers each shot separately; the host calculates and rolls each shot from
game data, caches the result for retransmits, and applies a hit through nv-rs's
combat runner. Its visibility estimate uses a ray to the target center. Guest
V.A.T.S. weapon and ammunition ownership are checked against the acknowledged
combat-inventory mirror. Protocol 16 adds a reliably acknowledged combat
profile; hit chance, AP cost, and damage calculation temporarily use that
guest's actor values and perk ranks. A shared death event includes the guest
ID for host-resolved V.A.T.S., hitscan, melee, bolt, and thrown-weapon kills.
Delayed bolts and thrown explosives retain the guest's launch-time combat
profile through impact. Kill rewards, karma, stats, ragdoll nudge, and
profile-changing script side effects still run through the shared `PLAYER_REF`
and do not persist as guest-owned changes. The host checks finite values and loaded perk records,
but a peer can still falsify its reported character stats because UDP peers
are unauthenticated.
Protocol 13 also routes guest hitscan shots and melee swings through the host,
which resolves them against current cell geometry and applies actor damage to
shared host state. Aim, weapon, and selected ammo remain guest reports, but the
host requires each to match the guest's latest acknowledged combat-inventory
snapshot. It decrements ammo counts after accepted shots. The host applies the
guest's acknowledged combat profile during damage calculations. Direct combat
death events include a guest ID, while game kill rewards and profile-changing
script effects still use the host's single `PLAYER_REF`. Flying
bolts and thrown weapons resolve on the host; the host tracks the guest's
thrown count and the guest consumes its local item only after acceptance.
Actor health replicates through the existing world journal
and keyframes, but guest hit effects and attack cancellation are not
reconciled. The host replicates loaded actors' nonfatal health alongside
death/destruction events and in keyframes.

The host's world journal repeats up to 80 recent events in movement snapshots.
On join or a gap, clients request a baseline containing loaded actor health
and current death/destruction flags. Keyframe chunks hold up to 130 variable-
size entries and are repeated until acknowledged, with a maximum of 4096
chunks. A guest's death/destruction update is acknowledged only when the same
value is already present in host state; the host never applies a claimed guest
mutation. Thus mutations from actions not yet simulated by the host do not
propagate. Death animation and ragdoll state are not replicated.

The host validates guest V.A.T.S. target eligibility against loaded actors,
range, line of sight, and both view angles. Input carries camera pitch and the
viewport aspect; the host bounds the aspect, derives the horizontal and
vertical frustum from the game's FOV, and checks the target's bounding sphere
from the player's eye point. These camera values remain client-reported, so
this is geometry validation rather than camera authentication. Each shot waits
for a fresh eligibility result. Dialogue updates report the current speaker,
an `INFO` record and response index, or up to eight `DIAL`/`INFO` choice pairs.
The host checks these references against loaded dialogue records, then runs the
same view caster used by the local crosshair with the guest's last reported
camera direction and host cell geometry. It accepts only a loaded, living
speaker that the pick selects. It forwards the nearest conversation within 512
game units as a read-only overlay, and listeners resolve the text from their
local game records. Reports expire when updates stop. Protocol 19 adds a
reliable acknowledged request carrying the selected topic/INFO pair and its
client-reported offer. The host validates the pair against loaded records and
the current speaker, then runs the selected begin script once. It runs an
active line's end script when the conversation advances or closes, or when
dialogue reports expire; a record-listed follow-up can also be mirrored from
a fresh dialogue snapshot.
Private-quest INFO scripts remain local. The host installs the peer's private
campaign facts, globals, reputation, and dialogue history while shared scripts
run, then restores its own values. Unconfigured globals remain shared. The host
recomputes the selected response against the guest's state, but does not
reproduce every detail of local menu presentation. Other `PLAYER_REF` script
effects remain shared-state risks. Each player keeps a local dialogue menu
and choice input. The world continues while a speaker is in the menu.
The host now orders shared quest stage, stage-done, objective, and status
changes, applies them to its `GameState`, and replays them to clients until
acknowledged. Reputation remains private. Quest progress is shared by default;
the host can mark faction-specific QUST records private with
`--coop-private-quests`. Scope is explicit because quest names and conditions
do not reliably identify a faction path. The host checks record IDs and runs
shared stage reports through its script runner. Stage-done, objective, and
status reports propagate only when host state already matches. Clients still
initiate stage reports. Protocol 19 carries dialogue choices reliably and
quest script locals in campaign events. The host mirrors shared line scripts
while loading the peer's configured private quest state and preserving its
own player state. The host recomputes selected topic/INFO availability from
that peer's campaign and profile state but does not reproduce every detail of
local menu presentation or isolate every script effect. Stage execution uses
the host's single `PLAYER_REF`; non-quest script locals are not synchronized.
Each client retains up to 4096 pending events, with no recovery after overflow.
Outside co-op, the viewer keeps its original pause behavior.

## Implementation path

1. Add a reusable versioned wire codec in `src/lib.rs` and connect it to Bevy
   host/client sessions; the viewer can now render remote player capsules in
   one shared loaded cell. The codec and viewer support chunked world
   keyframes to recover loaded actor health and death/destruction state after
   a join or event gap.
2. Send guest movement through the host's existing character collision
   controller, then reconcile each guest toward the host's 20 Hz pose snapshots.
   A macOS movement run was exercised on protocol 7; the current V.A.T.S.,
   hitscan, and melee host damage paths and health convergence still need a
   live check.
3. Keep client campaign changes in a per-peer host ledger; shared-scope quest
   stages, stage completion, objectives, and status now replay from a shared
   ledger, while the host can keep selected faction quests private. Split the
   rest of player-owned quest/faction and character data from shared world
   state, then pass `PlayerId` through player-initiated scripts and actions.
4. Keep world time running during local V.A.T.S., dialogue, and menus (the
   co-op frame and clock policy is in place); nearby players can now listen to
   a read-only dialogue overlay while the speaker keeps local choice control.
   V.A.T.S., hitscan, melee, and projectile damage now run on the host; verify
   them and the speaker's vulnerability live, then expand the combat-inventory
   mirror into full player-owned inventory and combat attribution, and isolate
   dialogue conditions and remaining `PLAYER_REF` script effects by player.
5. Extend world replication beyond actor health and death/destruction flags,
   then add authoritative projectiles, item transfers, moving physics frames,
   and world deltas; expand from one cell to multiple active regions and
   support four players.

This path is not complete until the same session is exercised with multiple
viewer instances and checks show that shared-world changes converge while
player-owned faction progress stays separate.
