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
  nv-rs combat runner and replicates actor health. The effective chance still
  comes from the client, and combat uses the host's single `PLAYER_REF` stats
  because player character state is not split yet. General projectile attacks
  remain client-side. No world pause or client-declared hit result.
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
choices. The client now reports quest stages, stage completion, objective
completion, quest status, and faction fame/infamy to a host ledger stored per
peer. That ledger is not shared with other clients, and the host does not yet
use it to run guest quest scripts or AI. Guest movement still uses the host's
player stats and movement settings. The viewer wires movement, V.A.T.S.,
dialogue, AI, and many script
conditions to one `PLAYER_REF`; `world::scripting::GameState` also mixes world
facts with player-specific facts. Bevy's ECS is a good fit for separate player
entities and player-keyed campaign resources, but nv-rs's systems still need a
player context and explicit state scope to use those entities consistently.

In co-op mode, frame scheduling keeps world and AI gates open while a local
menu or dialogue overlay is up, while the local player's movement gate and
interface still see the overlay. V.A.T.S. leaves virtual time at normal speed.
The host validates each queued attack's weapon data, kind, selected body part,
reload surcharge, and AP price, then reserves AP once. Version 10 numbers each
shot separately; the host rolls each shot using the effective probability the
guest reported, caches the result for retransmits, and applies a hit through
nv-rs's combat runner. It replicates loaded actors' nonfatal health alongside
death/destruction events and in keyframes. AP capacity, combat perks, and
damage calculations still use the host's single `PLAYER_REF`, and the chance
is client-reported. Guest weapon ownership is not checked. General weapons and
projectiles still resolve locally; kill rewards and attack cancellation are
not reconciled.

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
for a fresh eligibility result. Dialogue updates send the current speaker name, spoken
line, and up to eight offered topics; the host forwards the nearest
conversation within 512 game units as a read-only overlay at 10 Hz. Each
player keeps their own dialogue menu and choice input. The world continues
while a speaker is in the menu, but dialogue outcomes are not host-authoritative.
Campaign reports remain in each peer's host ledger and are not shared with
other players, scripts, or AI. The host validates quest, stage, objective, and
reputation IDs but not causal transitions. Reports omit globals and quest
script variables; each client retains up to 4096 pending events, with no
recovery after overflow. Outside co-op, the viewer keeps its original pause
behavior.

## Implementation path

1. Add a reusable versioned wire codec in `src/lib.rs` and connect it to Bevy
   host/client sessions; the viewer can now render remote player capsules in
   one shared loaded cell. The codec and viewer support chunked world
   keyframes to recover loaded actor health and death/destruction state after
   a join or event gap.
2. Send guest movement through the host's existing character collision
   controller, then reconcile each guest toward the host's 20 Hz pose snapshots.
   A macOS movement run was exercised on protocol 7; protocol 10 aim geometry,
   host damage, and health convergence still need a live check.
3. Keep client campaign changes in a per-peer host ledger (quest stages,
   stage completion, status, and reputation are now mirrored); split the rest
   of player-owned quest/faction and character data from shared world state,
   then pass `PlayerId` through player-initiated scripts and actions.
4. Keep world time running during local V.A.T.S., dialogue, and menus (the
   co-op frame and clock policy is in place); nearby players can now listen to
   a read-only dialogue overlay while the speaker keeps local choice control.
   V.A.T.S. damage now runs on the host; verify it and the speaker's
   vulnerability live, then move dialogue outcomes and normal combat to the
   host.
5. Extend world replication beyond actor health and death/destruction flags,
   then add authoritative
   combat, projectiles, item transfers, moving physics frames, and world
   deltas; expand from one cell to multiple active regions and support four
   players.

This path is not complete until the same session is exercised with multiple
viewer instances and checks show that shared-world changes converge while
player-owned faction progress stays separate.
