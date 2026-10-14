# Local co-op experiment

This is a local-only experiment for a 2–4 player Fallout: New Vegas co-op
mode in nv-rs. Its design and packet demo live under `experiments/coop-sync`;
the first host/client movement integration lives in the Bevy viewer. It is
kept on the local `codex/coop-experiment` branch with no remote branch or pull
request.

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
Protocol version 10 carries pitch and viewport aspect for aim validation, as
well as a shot index and host hit/miss result for each shot.
The host rolls from the effective probability reported by the client and
applies V.A.T.S. damage through nv-rs's combat runner against shared host
state. Actor-health events and keyframes reconcile nonfatal damage on guests.
Aim direction and viewport aspect are client-reported, so this check validates
target geometry but does not authenticate a camera. The chance still comes
from the client; player-specific weapon ownership,
skills, perks, and AP limits are not represented, so V.A.T.S. calculations use
the host's character state. Normal weapon attacks, kill rewards, cancellation
recovery, projectiles, inventory, moving platforms, and AI state are not yet
synchronized. Each client sends quest and faction progress reports to the host,
which stores them on that peer's ledger and acknowledges only the highest
contiguous event revision. Missing revisions remain pending for retransmission.
The host checks quest, stage, objective, and reputation IDs against loaded game
data, but does not validate causal quest transitions. These private reports are
not shared with other players, scripts, or AI. Death replication changes the
host-owned dead flag; it does not include death animation or ragdoll state.
World keyframes are limited to 4096 chunks of at most 130 variable-size entity
entries each; campaign updates are
retransmitted until acknowledged unless more than 4096 events accumulate in
the pending queue. The campaign report omits quest script variables, globals,
and other save data. Dialogue menus remain local; the host forwards a nearby
speaker's current line or offered topics in a read-only listener overlay within
512 game units. The speaker's local world simulation continues while the menu
is open; dialogue outcomes are not host-authoritative. Players
must start in the same cell with identical loaded plugin contents. The
handshake fingerprints plugin names and complete plugin files, but not
archives or loose assets. Cell transitions and Internet play are not
supported. UDP messages are unauthenticated.

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
correction under latency, or protocol version 10 behavior. Host damage,
nonfatal-health replication, keyframe recovery, dialogue listen-in, and
guest-update rejection still need live validation. Game audio was left enabled. A first launch exposed a
macOS Metal surface panic; the optional mailbox probe is now disabled on macOS
so Bevy keeps its default present mode.

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
