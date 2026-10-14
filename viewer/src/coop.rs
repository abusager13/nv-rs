//! Local host-authoritative player movement for the co-op experiment.
//!
//! This viewer slice is limited to peers in the same loaded cell. The host
//! simulates guest character controllers, broadcasts their poses, and owns
//! shared death/destruction state; guests predict movement locally and request
//! a complete world-flag keyframe when they join or detect a replay gap.

use std::collections::{HashMap, HashSet, VecDeque};
use std::io;
use std::net::{SocketAddr, UdpSocket};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use bevy::ecs::system::SystemParam;
use bevy::prelude::*;
use cellview::space;
use coop_sync_experiment::{
    sequence_is_newer, CampaignChange, CampaignEvent, DialogueSnapshot, Packet, PlayerId,
    PlayerInput, PlayerPose, WorldChange, WorldEvent, WorldStateEntry, MAX_CAMPAIGN_EVENTS,
    MAX_PACKET_BYTES, MAX_PLAYERS, MAX_VIEW_ASPECT, MAX_WORLD_EVENTS, MAX_WORLD_KEYFRAME_ENTRIES,
    MAX_WORLD_UPDATE_EVENTS, MIN_VIEW_ASPECT,
};
use esm::{FormId, LoadOrder};
use physics::{Character, CharacterShape, Person};
use world::locomotion::{self, SpeedSettings};

use crate::args::CoopMode;
use crate::dialogue::{Conversation, DialogueState};
use crate::menus::Menus;
use crate::walk::{self, CellCollision, Player};
use crate::{ai, GameFiles};

const NETWORK_TICK: Duration = Duration::from_millis(50);
const HELLO_RETRY: Duration = Duration::from_millis(500);
const PEER_TIMEOUT: Duration = Duration::from_secs(2);
const AIR_CONTROL: f32 = 0.3;
const MAX_PENDING_CAMPAIGN_EVENTS: usize = 4096;
const MAX_PENDING_WORLD_UPDATE_EVENTS: usize = 4096;
const WORLD_KEYFRAME_RETRY: Duration = Duration::from_millis(500);
const DIALOGUE_UPDATE_INTERVAL: Duration = Duration::from_millis(200);
const DIALOGUE_LISTEN_INTERVAL: Duration = Duration::from_millis(100);
const DIALOGUE_LISTEN_RADIUS: f32 = 512.0;

#[derive(Default)]
pub(crate) struct NetworkSettings {
    movement: Option<SpeedSettings>,
    vats: Option<world::vats::Settings>,
}

#[derive(SystemParam)]
pub(crate) struct NetworkAvatars<'w, 's> {
    commands: Commands<'w, 's>,
    meshes: ResMut<'w, Assets<Mesh>>,
    materials: ResMut<'w, Assets<StandardMaterial>>,
    avatars: Query<'w, 's, (Entity, &'static RemoteAvatar, &'static mut Transform)>,
}

#[derive(SystemParam)]
pub(crate) struct NetworkView<'w, 's> {
    player: ResMut<'w, Player>,
    cameras: Query<'w, 's, &'static crate::FlyCamera>,
    windows: Query<'w, 's, &'static Window, With<bevy::window::PrimaryWindow>>,
}

#[derive(Resource)]
pub struct Session {
    endpoint: Endpoint,
    scene_key: u64,
    tick: u32,
    accumulator: f32,
    next_send: Instant,
    next_hello: Instant,
    input_sequence: u32,
    dialogue_sequence: u32,
    last_dialogue_sequence: Option<u32>,
    next_dialogue_send: Instant,
    assigned_player: Option<PlayerId>,
    last_received_tick: Option<u32>,
    poses: HashMap<PlayerId, PlayerPose>,
    world_revision: u32,
    world_events: VecDeque<WorldEvent>,
    observed_dead: HashSet<u32>,
    observed_destroyed: HashSet<u32>,
    observed_health: HashMap<u32, i32>,
    world_update_revision: u32,
    world_update_events: VecDeque<WorldEvent>,
    observed_client_dead: HashSet<u32>,
    observed_client_destroyed: HashSet<u32>,
    client_world_initialized: bool,
    last_world_revision: Option<u32>,
    confirmed_dead: HashSet<u32>,
    confirmed_destroyed: HashSet<u32>,
    confirmed_health: HashMap<u32, i32>,
    world_keyframe: ClientWorldKeyframe,
    campaign_revision: u32,
    campaign_events: VecDeque<CampaignEvent>,
    observed_stages: HashMap<u32, u16>,
    observed_stages_done: HashSet<(u32, u16)>,
    observed_objectives: HashSet<(u32, i32)>,
    observed_quest_status: HashMap<u32, (bool, bool, bool)>,
    observed_reputations: HashMap<u32, (f32, f32)>,
    aim_sequence: u32,
    aim_target: Option<FormId>,
    last_aim_result_sequence: Option<u32>,
    validated_aim: Option<(FormId, Instant)>,
    next_aim_request: Instant,
    vats_attack_sequence: u32,
    vats_attack: Option<VatsAttackRequest>,
}

#[derive(Clone, Copy)]
struct VatsAttackRequest {
    sequence: u32,
    attack_id: u64,
    shot_index: u16,
    target: FormId,
    weapon: Option<FormId>,
    kind: u8,
    reload: bool,
    slot: i8,
    part_av: i32,
    action_points: f32,
    hit_chance: f32,
    decision: Option<bool>,
    hit: Option<bool>,
}

#[derive(Clone, Copy)]
struct HostVatsAttack {
    attack_id: u64,
    target: FormId,
    weapon: u32,
    kind: u8,
    ap_cost: f32,
    next_shot: u16,
}

struct ClientWorldKeyframe {
    next_transfer_id: u32,
    pending_transfer_id: Option<u32>,
    next_request: Instant,
    completed_transfer_id: Option<u32>,
    assembly: Option<WorldKeyframeAssembly>,
}

struct WorldKeyframeAssembly {
    transfer_id: u32,
    revision: u32,
    received: usize,
    chunks: Vec<Option<Vec<WorldStateEntry>>>,
}

struct HostWorldKeyframe {
    transfer_id: u32,
    revision: u32,
    next_chunk: u16,
    chunk_count: u16,
    entries: Vec<WorldStateEntry>,
}

impl HostWorldKeyframe {
    fn new(
        transfer_id: u32,
        revision: u32,
        dead: &HashSet<u32>,
        destroyed: &HashSet<u32>,
        order: &esm::LoadOrder,
        state: &world::scripting::GameState,
        walkers: &Query<&ai::Walker>,
    ) -> Option<Self> {
        let mut entries = HashMap::<u32, (bool, bool, Option<i32>)>::new();
        for reference in dead.iter().filter(|reference| **reference != 0) {
            entries.entry(*reference).or_default().0 = true;
        }
        for reference in destroyed.iter().filter(|reference| **reference != 0) {
            entries.entry(*reference).or_default().1 = true;
        }
        for walker in walkers.iter() {
            if walker.reference == world::dialogue::PLAYER_REF {
                continue;
            }
            if let Some(health) = actor_health_milli(order, state, walker.reference) {
                entries.entry(walker.reference.0).or_default().2 = Some(health);
            }
        }
        let mut entries: Vec<_> = entries
            .into_iter()
            .map(
                |(reference, (dead, destroyed, health_milli))| WorldStateEntry {
                    reference,
                    dead,
                    destroyed,
                    health_milli,
                },
            )
            .collect();
        entries.sort_unstable_by_key(|entry| entry.reference);
        let chunk_count = entries.len().div_ceil(MAX_WORLD_KEYFRAME_ENTRIES).max(1);
        if chunk_count > coop_sync_experiment::MAX_WORLD_KEYFRAME_CHUNKS {
            return None;
        }
        Some(Self {
            transfer_id,
            revision,
            next_chunk: 0,
            chunk_count: chunk_count as u16,
            entries,
        })
    }

    fn next_packet(&mut self) -> Packet {
        let index = self.next_chunk;
        let start = usize::from(index) * MAX_WORLD_KEYFRAME_ENTRIES;
        let end = (start + MAX_WORLD_KEYFRAME_ENTRIES).min(self.entries.len());
        let entries = self.entries[start..end].to_vec();
        self.next_chunk = (self.next_chunk + 1) % self.chunk_count;
        Packet::WorldKeyframeChunk {
            transfer_id: self.transfer_id,
            revision: self.revision,
            index,
            count: self.chunk_count,
            entries,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum VatsAttackApproval {
    Pending,
    Accepted { hit: Option<bool> },
    Rejected,
}

/// Bevy resources must be concrete types; the network session is optional
/// when the viewer starts without either co-op command-line flag.
#[derive(Resource)]
pub struct CoOpSession(pub Option<Session>);

impl CoOpSession {
    pub fn is_active(&self) -> bool {
        self.0.is_some()
    }

    pub(crate) fn host_owns_damage(&self) -> bool {
        !matches!(
            self.0.as_ref().map(|session| &session.endpoint),
            Some(Endpoint::Client { .. })
        )
    }

    pub(crate) fn vats_attack_charged_by_host(&self, attack_id: u64) -> bool {
        let Some(session) = self.0.as_ref() else {
            return false;
        };
        matches!(
            &session.endpoint,
            Endpoint::Client { .. }
                if session.vats_attack.is_some_and(|request| {
                    request.attack_id == attack_id && request.decision == Some(true)
                })
        )
    }

    pub fn target_is_validated(&self, target: FormId) -> bool {
        let Some(session) = self.0.as_ref() else {
            return true;
        };
        match &session.endpoint {
            Endpoint::Host { .. } => true,
            Endpoint::Client { .. } => {
                session.aim_target == Some(target)
                    && session.validated_aim.is_some_and(|(approved, at)| {
                        approved == target && at.elapsed() <= Duration::from_millis(250)
                    })
            }
        }
    }

    pub(crate) fn request_vats_attack(
        &mut self,
        attack_id: u64,
        shot_index: u16,
        target: FormId,
        weapon: Option<FormId>,
        kind: u8,
        reload: bool,
        slot: i8,
        part_av: i32,
        action_points: f32,
        hit_chance: f32,
    ) -> VatsAttackApproval {
        if !action_points.is_finite()
            || action_points <= 0.0
            || !hit_chance.is_finite()
            || !(0.0..=1.0).contains(&hit_chance)
        {
            return VatsAttackApproval::Rejected;
        }
        let Some(session) = self.0.as_mut() else {
            return VatsAttackApproval::Accepted { hit: None };
        };
        match &session.endpoint {
            Endpoint::Host { .. } => VatsAttackApproval::Accepted { hit: None },
            Endpoint::Client { .. } => {
                if let Some(request) = session.vats_attack {
                    if request.attack_id == attack_id
                        && request.shot_index == shot_index
                        && request.target == target
                    {
                        return match request.decision {
                            Some(true) => VatsAttackApproval::Accepted { hit: request.hit },
                            Some(false) => VatsAttackApproval::Rejected,
                            None => VatsAttackApproval::Pending,
                        };
                    }
                }
                session.vats_attack_sequence = session.vats_attack_sequence.wrapping_add(1);
                session.vats_attack = Some(VatsAttackRequest {
                    sequence: session.vats_attack_sequence,
                    attack_id,
                    shot_index,
                    target,
                    weapon,
                    kind,
                    reload,
                    slot,
                    part_av,
                    action_points,
                    hit_chance,
                    decision: None,
                    hit: None,
                });
                VatsAttackApproval::Pending
            }
        }
    }
}

pub(crate) fn remote_people(session: &CoOpSession) -> Vec<Person> {
    let Some(session) = session.0.as_ref() else {
        return Vec::new();
    };
    match &session.endpoint {
        Endpoint::Host { peers, .. } => peers
            .values()
            .map(|peer| person(peer.character.feet))
            .collect(),
        Endpoint::Client { .. } => session
            .poses
            .values()
            .filter(|pose| Some(pose.id) != session.assigned_player)
            .map(|pose| person(pose.position))
            .collect(),
    }
}

fn person(feet: [f32; 3]) -> Person {
    Person {
        feet,
        radius: CharacterShape::PLAYER.radius,
        height: CharacterShape::PLAYER.height,
    }
}

enum Endpoint {
    Host {
        socket: UdpSocket,
        peers: HashMap<SocketAddr, Peer>,
    },
    Client {
        socket: UdpSocket,
        server: SocketAddr,
    },
}

struct Peer {
    player: PlayerId,
    character: Character,
    spawn_feet: [f32; 3],
    reported_nonzero_input: bool,
    reported_host_movement: bool,
    input: PlayerInput,
    last_input_sequence: Option<u32>,
    last_seen: Instant,
    campaign: CampaignLedger,
    last_world_update_revision: Option<u32>,
    last_aim_sequence: Option<u32>,
    action_points: f32,
    max_action_points: f32,
    last_vats_activity: Option<Instant>,
    last_attack_sequence: Option<u32>,
    last_attack_result: Option<(u32, u64, u16, bool, bool)>,
    vats_attack: Option<HostVatsAttack>,
    last_world_keyframe_request: Option<u32>,
    world_keyframe: Option<HostWorldKeyframe>,
    last_dialogue_sequence: Option<u32>,
    dialogue: Option<DialogueSnapshot>,
}

/// The host keeps each guest's private campaign report under that peer only.
/// This is a progress mirror; the host has not yet validated script causality.
#[derive(Default)]
struct CampaignLedger {
    last_revision: Option<u32>,
    stages: HashMap<u32, u16>,
    stages_done: HashSet<(u32, u16)>,
    objectives: HashSet<(u32, i32)>,
    quest_status: HashMap<u32, (bool, bool, bool)>,
    reputations: HashMap<u32, (f32, f32)>,
}

#[derive(Component)]
pub(crate) struct RemoteAvatar(PlayerId);

pub fn scene_key(cell: &str, order: &LoadOrder) -> u64 {
    // Stable FNV-1a over the normalized starting-cell argument and exact
    // loaded plugin contents. Peers with different FormID spaces or plugin
    // data must not exchange shared-world references.
    let mut hash = 0xcbf29ce484222325u64;
    for byte in cell.trim().to_ascii_lowercase().bytes() {
        hash = fingerprint_byte(hash, byte);
    }
    hash = fingerprint_byte(hash, 0);
    for plugin in order.plugins() {
        for byte in plugin.name.to_ascii_lowercase().bytes() {
            hash = fingerprint_byte(hash, byte);
        }
        hash = fingerprint_byte(hash, 0);
        for byte in plugin.plugin.content_fingerprint().to_le_bytes() {
            hash = fingerprint_byte(hash, byte);
        }
    }
    hash
}

fn fingerprint_byte(hash: u64, byte: u8) -> u64 {
    (hash ^ u64::from(byte)).wrapping_mul(0x100000001b3)
}

impl Session {
    pub fn open(mode: Option<CoopMode>, scene_key: u64) -> io::Result<Option<Self>> {
        let Some(mode) = mode else {
            return Ok(None);
        };
        let world_keyframe_seed = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |time| time.as_micros() as u32)
            ^ std::process::id().rotate_left(16);
        let endpoint = match mode {
            CoopMode::Host(address) => {
                let socket = UdpSocket::bind(address)?;
                socket.set_nonblocking(true)?;
                println!("Co-op host listening on {}", socket.local_addr()?);
                Endpoint::Host {
                    socket,
                    peers: HashMap::new(),
                }
            }
            CoopMode::Join(server) => {
                let socket = UdpSocket::bind("0.0.0.0:0")?;
                socket.set_nonblocking(true)?;
                println!("Joining co-op host at {server}");
                Endpoint::Client { socket, server }
            }
        };
        let now = Instant::now();
        Ok(Some(Self {
            endpoint,
            scene_key,
            tick: 0,
            accumulator: 0.0,
            next_send: now,
            next_hello: now,
            input_sequence: 0,
            dialogue_sequence: 0,
            last_dialogue_sequence: None,
            next_dialogue_send: now,
            assigned_player: None,
            last_received_tick: None,
            poses: HashMap::new(),
            world_revision: 0,
            world_events: VecDeque::new(),
            observed_dead: HashSet::new(),
            observed_destroyed: HashSet::new(),
            observed_health: HashMap::new(),
            world_update_revision: 0,
            world_update_events: VecDeque::new(),
            observed_client_dead: HashSet::new(),
            observed_client_destroyed: HashSet::new(),
            client_world_initialized: false,
            last_world_revision: None,
            confirmed_dead: HashSet::new(),
            confirmed_destroyed: HashSet::new(),
            confirmed_health: HashMap::new(),
            world_keyframe: ClientWorldKeyframe {
                next_transfer_id: world_keyframe_seed,
                pending_transfer_id: None,
                next_request: now,
                completed_transfer_id: None,
                assembly: None,
            },
            campaign_revision: 0,
            campaign_events: VecDeque::new(),
            observed_stages: HashMap::new(),
            observed_stages_done: HashSet::new(),
            observed_objectives: HashSet::new(),
            observed_quest_status: HashMap::new(),
            observed_reputations: HashMap::new(),
            aim_sequence: 0,
            aim_target: None,
            last_aim_result_sequence: None,
            validated_aim: None,
            next_aim_request: now,
            vats_attack_sequence: 0,
            vats_attack: None,
        }))
    }
}

#[allow(clippy::too_many_arguments)]
pub fn update_network(
    mut session: ResMut<CoOpSession>,
    real_time: Res<Time<Real>>,
    view: NetworkView,
    mut collision: ResMut<CellCollision>,
    game: Res<GameFiles>,
    scripts: Res<crate::scripts::Scripts>,
    exterior: Option<Res<crate::exterior::Exterior>>,
    mut state: ResMut<DialogueState>,
    mut listener_dialogue: ResMut<crate::dialogue::ListenerDialogue>,
    walkers: Query<&ai::Walker>,
    nav: Option<Res<ai::CellNav>>,
    menus: Res<Menus>,
    conversation: Res<Conversation>,
    vats: Res<crate::vats::Vats>,
    mut settings: Local<NetworkSettings>,
    mut avatars: NetworkAvatars,
) {
    let Some(session) = session.0.as_mut() else {
        return;
    };
    let dt = real_time.delta_secs().min(0.25);
    let now = Instant::now();
    let (view_heading, view_pitch) = view
        .cameras
        .iter()
        .next()
        .map_or((view.player.network_heading, 0.0), |camera| {
            (camera.yaw, camera.pitch)
        });
    let view_aspect = view
        .windows
        .single()
        .map(|window| {
            let (width, height) = (window.width(), window.height());
            if width.is_finite() && height.is_finite() && width > 0.0 && height > 0.0 {
                (width / height).clamp(MIN_VIEW_ASPECT, MAX_VIEW_ASPECT)
            } else {
                16.0 / 9.0
            }
        })
        .unwrap_or(16.0 / 9.0);
    let mut player = view.player;
    let local_dialogue = conversation.coop_snapshot();
    let mut incoming = [0u8; MAX_PACKET_BYTES];

    if matches!(session.endpoint, Endpoint::Client { .. }) {
        if !session.client_world_initialized && session.last_world_revision.is_some() {
            session.observed_client_dead = state
                .0
                .dead
                .iter()
                .filter(|reference| **reference != world::dialogue::PLAYER_REF)
                .map(|reference| reference.0)
                .collect();
            session.observed_client_destroyed = state
                .0
                .destroyed
                .iter()
                .map(|reference| reference.0)
                .collect();
            session.client_world_initialized = true;
        }
        capture_campaign_changes(
            &mut session.campaign_revision,
            &mut session.campaign_events,
            &mut session.observed_stages,
            &mut session.observed_stages_done,
            &mut session.observed_objectives,
            &mut session.observed_quest_status,
            &mut session.observed_reputations,
            &state.0,
        );
        if session.client_world_initialized {
            capture_world_changes(
                &game.0.order,
                &walkers,
                &mut session.world_update_revision,
                &mut session.world_update_events,
                &mut session.observed_client_dead,
                &mut session.observed_client_destroyed,
                &mut session.observed_health,
                &state.0,
                MAX_PENDING_WORLD_UPDATE_EVENTS,
                false,
                false,
            );
        }
    }

    if matches!(session.endpoint, Endpoint::Host { .. }) {
        capture_world_changes(
            &game.0.order,
            &walkers,
            &mut session.world_revision,
            &mut session.world_events,
            &mut session.observed_dead,
            &mut session.observed_destroyed,
            &mut session.observed_health,
            &state.0,
            MAX_WORLD_EVENTS,
            true,
            true,
        );
    }

    if matches!(session.endpoint, Endpoint::Client { .. }) {
        let target = vats.selected_target();
        if session.aim_target != target {
            session.aim_target = target;
            session.validated_aim = None;
            session.next_aim_request = now;
        }
    }

    match &mut session.endpoint {
        Endpoint::Host { socket, peers } => {
            let NetworkSettings { movement, vats } = &mut *settings;
            let speed_settings = movement.get_or_insert_with(|| SpeedSettings::read(&game.0.order));
            let vats_settings =
                vats.get_or_insert_with(|| world::vats::Settings::load(&game.0.order));
            poll_host(
                socket,
                peers,
                session.scene_key,
                player.character.feet,
                &game.0.order,
                &scripts.0,
                vats_settings,
                &mut state.0,
                &mut session.world_revision,
                &mut session.world_events,
                &mut session.observed_dead,
                &mut session.observed_destroyed,
                &collision.0,
                &walkers,
                exterior.as_deref(),
                &mut incoming,
            );
            listener_dialogue.0 = if local_dialogue.is_some() {
                None
            } else {
                nearby_dialogue(player.character.feet, PlayerId(0), None, peers, &walkers)
            };
            regenerate_peer_action_points(peers, vats_settings, &game.0.order, &state.0, dt, now);
            session.accumulator += dt;
            while session.accumulator >= NETWORK_TICK.as_secs_f32() {
                step_peers(
                    peers,
                    &mut collision.0,
                    speed_settings,
                    &game,
                    &state.0,
                    nav.as_deref(),
                    &walkers,
                    player.character.feet,
                );
                session.accumulator -= NETWORK_TICK.as_secs_f32();
                session.tick = session.tick.wrapping_add(1);
            }
            if now >= session.next_send {
                session.next_send = now + NETWORK_TICK;
                let mut poses = Vec::with_capacity(peers.len() + 1);
                poses.push(PlayerPose {
                    id: PlayerId(0),
                    position: player.character.feet,
                    heading: player.network_heading,
                });
                poses.extend(peers.values().map(|peer| PlayerPose {
                    id: peer.player,
                    position: peer.character.feet,
                    heading: peer.input.heading,
                }));
                poses.sort_by_key(|pose| pose.id);
                session.poses = poses.iter().map(|pose| (pose.id, *pose)).collect();
                for (address, peer) in peers.iter_mut() {
                    let packet = Packet::Snapshot {
                        tick: session.tick,
                        assigned_player: peer.player,
                        players: poses.clone(),
                        world_revision: session.world_revision,
                        campaign_revision: peer.campaign.last_revision.unwrap_or_default(),
                        world_update_revision: peer.last_world_update_revision.unwrap_or_default(),
                        world_events: session.world_events.iter().copied().collect(),
                    };
                    let _ = send(socket, &packet, *address);
                    if let Some(keyframe) = peer.world_keyframe.as_mut() {
                        let _ = send(socket, &keyframe.next_packet(), *address);
                    }
                }
            }
            if now >= session.next_dialogue_send {
                session.next_dialogue_send = now + DIALOGUE_LISTEN_INTERVAL;
                session.dialogue_sequence = session.dialogue_sequence.wrapping_add(1);
                let sequence = session.dialogue_sequence;
                for (address, peer) in peers.iter() {
                    let (player, dialogue) = nearby_dialogue(
                        peer.character.feet,
                        peer.player,
                        local_dialogue.as_ref(),
                        peers,
                        &walkers,
                    )
                    .map_or((PlayerId(0), None), |(player, dialogue)| {
                        (player, Some(dialogue))
                    });
                    let _ = send(
                        socket,
                        &Packet::DialogueListen {
                            player,
                            sequence,
                            dialogue,
                        },
                        *address,
                    );
                }
            }
            peers.retain(|address, peer| {
                let active = now.duration_since(peer.last_seen) <= PEER_TIMEOUT;
                if !active {
                    println!("Co-op player {} disconnected ({address})", peer.player.0);
                }
                active
            });
            collision.0.set_people(world_people(
                &walkers,
                &state.0,
                peers,
                player.character.feet,
                None,
            ));
        }
        Endpoint::Client { socket, server } => {
            poll_client(
                socket,
                *server,
                session.scene_key,
                &game.0.order,
                &mut session.assigned_player,
                &mut session.last_received_tick,
                &mut session.poses,
                &mut session.last_world_revision,
                &mut session.confirmed_dead,
                &mut session.confirmed_destroyed,
                &mut session.confirmed_health,
                &mut session.world_keyframe,
                &mut session.last_dialogue_sequence,
                &mut listener_dialogue,
                &mut session.campaign_events,
                &mut session.world_update_events,
                &mut session.observed_client_dead,
                &mut session.observed_client_destroyed,
                &mut session.client_world_initialized,
                session.aim_target,
                &mut session.last_aim_result_sequence,
                &mut session.validated_aim,
                &mut session.vats_attack,
                &mut state.0,
                now,
                &mut incoming,
            );
            let blocked = menus.is_open()
                || conversation
                    .0
                    .as_ref()
                    .is_some_and(|talk| !talk.is_line_only())
                || vats.is_on();
            if now >= session.next_hello && session.assigned_player.is_none() {
                let _ = send(
                    socket,
                    &Packet::Hello {
                        scene_key: session.scene_key,
                    },
                    *server,
                );
                session.next_hello = now + HELLO_RETRY;
            }
            if now >= session.next_send {
                session.next_send = now + NETWORK_TICK;
                session.input_sequence = session.input_sequence.wrapping_add(1);
                let input = PlayerInput {
                    sequence: session.input_sequence,
                    axes: if blocked {
                        [0.0; 2]
                    } else {
                        player.network_axes
                    },
                    heading: view_heading,
                    pitch: view_pitch,
                    view_aspect,
                    running: !blocked && player.network_running,
                    sneaking: player.network_sneaking,
                };
                let _ = send(socket, &Packet::Input(input), *server);
                if session.assigned_player.is_some() && now >= session.next_dialogue_send {
                    session.next_dialogue_send = now + DIALOGUE_UPDATE_INTERVAL;
                    session.dialogue_sequence = session.dialogue_sequence.wrapping_add(1);
                    let _ = send(
                        socket,
                        &Packet::DialogueUpdate {
                            sequence: session.dialogue_sequence,
                            dialogue: local_dialogue.clone(),
                        },
                        *server,
                    );
                }
                if let Some(transfer_id) = session.world_keyframe.pending_transfer_id {
                    if now >= session.world_keyframe.next_request {
                        let _ = send(
                            socket,
                            &Packet::WorldKeyframeRequest {
                                transfer_id,
                                after_revision: session.last_world_revision.unwrap_or_default(),
                            },
                            *server,
                        );
                        session.world_keyframe.next_request = now + WORLD_KEYFRAME_RETRY;
                    }
                }
                if session.assigned_player.is_some() && !session.campaign_events.is_empty() {
                    let packet = Packet::CampaignUpdate {
                        events: session
                            .campaign_events
                            .iter()
                            .take(MAX_CAMPAIGN_EVENTS)
                            .copied()
                            .collect(),
                    };
                    let _ = send(socket, &packet, *server);
                }
                if session.assigned_player.is_some() && !session.world_update_events.is_empty() {
                    let packet = Packet::WorldUpdate {
                        events: session
                            .world_update_events
                            .iter()
                            .take(MAX_WORLD_UPDATE_EVENTS)
                            .copied()
                            .collect(),
                    };
                    let _ = send(socket, &packet, *server);
                }
                if let (Some(_), Some(request)) = (
                    session.assigned_player,
                    session
                        .vats_attack
                        .filter(|request| request.decision.is_none()),
                ) {
                    let _ = send(
                        socket,
                        &Packet::VatsAttack {
                            sequence: request.sequence,
                            attack_id: request.attack_id,
                            shot_index: request.shot_index,
                            target: request.target.0,
                            weapon: request.weapon.map_or(0, |weapon| weapon.0),
                            kind: request.kind,
                            reload: request.reload,
                            slot: request.slot,
                            part_av: request.part_av,
                            action_points: request.action_points,
                            hit_chance: request.hit_chance,
                        },
                        *server,
                    );
                }
            }
            if let Some(assigned) = session.assigned_player {
                if let Some(authoritative) = session.poses.get(&assigned) {
                    reconcile_local(&mut player, *authoritative, dt);
                }
            }
            if let (Some(_assigned), Some(target)) = (session.assigned_player, session.aim_target) {
                if now >= session.next_aim_request {
                    session.aim_sequence = session.aim_sequence.wrapping_add(1);
                    let _ = send(
                        socket,
                        &Packet::VatsAim {
                            sequence: session.aim_sequence,
                            target: target.0,
                        },
                        *server,
                    );
                    session.next_aim_request = now + Duration::from_millis(100);
                }
            }
            collision.0.set_people(world_people_from_poses(
                &walkers,
                &state.0,
                &session.poses,
                session.assigned_player,
            ));
        }
    }

    update_avatars(
        &mut avatars.commands,
        &mut avatars.meshes,
        &mut avatars.materials,
        &mut avatars.avatars,
        &session.poses,
        session.assigned_player.unwrap_or(PlayerId(0)),
        dt,
    );
}

fn regenerate_peer_action_points(
    peers: &mut HashMap<SocketAddr, Peer>,
    settings: &world::vats::Settings,
    order: &esm::LoadOrder,
    state: &world::scripting::GameState,
    dt: f32,
    now: Instant,
) {
    let perk = world::perks::apply(order, state, world::perks::entry::ACTION_POINT_REGEN, 1.0);
    for peer in peers.values_mut() {
        let vats_active = peer
            .last_vats_activity
            .is_some_and(|at| now.duration_since(at) <= Duration::from_millis(250));
        if !vats_active {
            let regained = settings.restore_rate * perk * peer.max_action_points * dt;
            peer.action_points = (peer.action_points + regained).min(peer.max_action_points);
        }
    }
}

fn poll_host(
    socket: &UdpSocket,
    peers: &mut HashMap<SocketAddr, Peer>,
    scene_key: u64,
    start: [f32; 3],
    order: &esm::LoadOrder,
    scripts: &world::scripting::ScriptCache,
    vats_settings: &world::vats::Settings,
    state: &mut world::scripting::GameState,
    world_revision: &mut u32,
    world_events: &mut VecDeque<WorldEvent>,
    observed_dead: &mut HashSet<u32>,
    observed_destroyed: &mut HashSet<u32>,
    collision: &physics::Collider,
    walkers: &Query<&ai::Walker>,
    exterior: Option<&crate::exterior::Exterior>,
    incoming: &mut [u8; MAX_PACKET_BYTES],
) {
    let mut campaign_quest_schemas = HashMap::new();
    let mut loaded_references = None;
    loop {
        let (length, address) = match socket.recv_from(incoming) {
            Ok(result) => result,
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => break,
            Err(error) => {
                eprintln!("Co-op receive failed: {error}");
                break;
            }
        };
        let Ok(packet) = Packet::decode(&incoming[..length]) else {
            continue;
        };
        match packet {
            Packet::Hello {
                scene_key: peer_scene,
            } if peer_scene == scene_key => {
                let was_present = peers.contains_key(&address);
                let max_action_points = world::vats::max_action_points(order, state).max(0.0);
                let Some(id) = add_peer(peers, address, start, max_action_points) else {
                    continue;
                };
                if !was_present {
                    println!("Co-op player {} joined from {address}", id.0);
                }
                let _ = send(
                    socket,
                    &Packet::Welcome {
                        player: id,
                        scene_key,
                    },
                    address,
                );
            }
            Packet::Input(input) => {
                if let Some(peer) = peers.get_mut(&address) {
                    peer.last_seen = Instant::now();
                    if peer
                        .last_input_sequence
                        .map_or(true, |previous| sequence_is_newer(input.sequence, previous))
                    {
                        peer.last_input_sequence = Some(input.sequence);
                        peer.input = input;
                        if !peer.reported_nonzero_input && input.axes != [0.0; 2] {
                            println!(
                                "Co-op player {} sent movement input: axes=({:.2}, {:.2})",
                                peer.player.0, input.axes[0], input.axes[1]
                            );
                            peer.reported_nonzero_input = true;
                        }
                    }
                }
            }
            Packet::CampaignUpdate { events } => {
                if let Some(peer) = peers.get_mut(&address) {
                    peer.last_seen = Instant::now();
                    apply_campaign_events(
                        &mut peer.campaign,
                        &events,
                        order,
                        &mut campaign_quest_schemas,
                    );
                }
            }
            Packet::WorldUpdate { events } => {
                if let Some(peer) = peers.get_mut(&address) {
                    peer.last_seen = Instant::now();
                    apply_client_world_events(
                        &mut peer.last_world_update_revision,
                        &events,
                        order,
                        state,
                        walkers,
                        exterior,
                        &mut loaded_references,
                        world_revision,
                        world_events,
                        observed_dead,
                        observed_destroyed,
                    );
                }
            }
            Packet::DialogueUpdate { sequence, dialogue } => {
                if let Some(peer) = peers.get_mut(&address) {
                    peer.last_seen = Instant::now();
                    if peer
                        .last_dialogue_sequence
                        .map_or(true, |previous| sequence_is_newer(sequence, previous))
                    {
                        peer.last_dialogue_sequence = Some(sequence);
                        peer.dialogue = dialogue;
                    }
                }
            }
            Packet::WorldKeyframeRequest {
                transfer_id,
                after_revision: _,
            } => {
                if let Some(peer) = peers.get_mut(&address) {
                    peer.last_seen = Instant::now();
                    let is_current = peer.last_world_keyframe_request == Some(transfer_id);
                    let is_newer = peer
                        .last_world_keyframe_request
                        .is_none_or(|previous| sequence_is_newer(transfer_id, previous));
                    if !is_current && is_newer {
                        if let Some(transfer) = HostWorldKeyframe::new(
                            transfer_id,
                            *world_revision,
                            observed_dead,
                            observed_destroyed,
                            order,
                            state,
                            walkers,
                        ) {
                            peer.world_keyframe = Some(transfer);
                            peer.last_world_keyframe_request = Some(transfer_id);
                        }
                    }
                }
            }
            Packet::WorldKeyframeAck { transfer_id } => {
                if let Some(peer) = peers.get_mut(&address) {
                    peer.last_seen = Instant::now();
                    if peer
                        .world_keyframe
                        .as_ref()
                        .is_some_and(|transfer| transfer.transfer_id == transfer_id)
                    {
                        peer.world_keyframe = None;
                    }
                }
            }
            Packet::VatsAim { sequence, target } => {
                if let Some(peer) = peers.get_mut(&address) {
                    peer.last_seen = Instant::now();
                    peer.last_vats_activity = Some(Instant::now());
                    if peer
                        .last_aim_sequence
                        .map_or(true, |previous| sequence_is_newer(sequence, previous))
                    {
                        peer.last_aim_sequence = Some(sequence);
                        let accepted = validate_vats_target(
                            order,
                            vats_settings,
                            state,
                            collision,
                            walkers,
                            peer,
                            FormId(target),
                        );
                        let _ = send(
                            socket,
                            &Packet::VatsAimResult {
                                sequence,
                                target,
                                accepted,
                            },
                            address,
                        );
                    }
                }
            }
            Packet::VatsAttack {
                sequence,
                attack_id,
                shot_index,
                target,
                weapon,
                kind,
                reload,
                slot,
                part_av,
                action_points,
                hit_chance,
            } => {
                if let Some(peer) = peers.get_mut(&address) {
                    peer.last_seen = Instant::now();
                    peer.last_vats_activity = Some(Instant::now());
                    if let Some((cached_sequence, cached_id, cached_shot, accepted, hit)) =
                        peer.last_attack_result
                    {
                        if sequence == cached_sequence
                            && attack_id == cached_id
                            && shot_index == cached_shot
                        {
                            let _ = send(
                                socket,
                                &Packet::VatsAttackResult {
                                    sequence,
                                    attack_id,
                                    shot_index,
                                    accepted,
                                    action_points: peer.action_points,
                                    hit,
                                },
                                address,
                            );
                            continue;
                        }
                    }
                    if peer
                        .last_attack_sequence
                        .is_some_and(|previous| !sequence_is_newer(sequence, previous))
                    {
                        continue;
                    }
                    peer.last_attack_sequence = Some(sequence);
                    let target_valid = validate_vats_target(
                        order,
                        vats_settings,
                        state,
                        collision,
                        walkers,
                        peer,
                        FormId(target),
                    );
                    let target_down = state.more.down.contains_key(&FormId(target))
                        || walkers
                            .iter()
                            .find(|walker| walker.reference == FormId(target))
                            .is_some_and(ai::Walker::is_fallen);
                    let expected_cost = expected_vats_attack_cost(
                        order,
                        state,
                        vats_settings,
                        target_down,
                        FormId(weapon),
                        kind,
                        reload,
                    );
                    let part_valid = valid_vats_part(order, state, FormId(target), slot, part_av);
                    let mut accepted = false;
                    let mut hit = false;
                    if let Some(cost) = expected_cost.filter(|cost| cost.is_finite() && *cost > 0.0)
                    {
                        let tolerance = (cost * 0.01).max(0.05);
                        let cost_matches =
                            action_points.is_finite() && (action_points - cost).abs() <= tolerance;
                        if shot_index == 0 {
                            accepted = target_valid
                                && part_valid
                                && cost_matches
                                && cost <= peer.action_points
                                && cost <= peer.max_action_points
                                && peer
                                    .vats_attack
                                    .is_none_or(|active| active.attack_id != attack_id);
                            if accepted {
                                peer.action_points = (peer.action_points - cost).max(0.0);
                                hit = roll_vats_hit(state, hit_chance);
                                peer.vats_attack = Some(HostVatsAttack {
                                    attack_id,
                                    target: FormId(target),
                                    weapon,
                                    kind,
                                    ap_cost: cost,
                                    next_shot: 1,
                                });
                            }
                        } else if let Some(active) = peer.vats_attack.as_mut() {
                            accepted = target_valid
                                && part_valid
                                && active.attack_id == attack_id
                                && active.target == FormId(target)
                                && active.weapon == weapon
                                && active.kind == kind
                                && active.next_shot == shot_index
                                && (active.ap_cost - cost).abs() <= tolerance
                                && cost_matches;
                            if accepted {
                                hit = roll_vats_hit(state, hit_chance);
                                active.next_shot = active.next_shot.wrapping_add(1);
                            }
                        }
                    }
                    if accepted && hit {
                        apply_host_vats_shot(
                            order,
                            scripts,
                            state,
                            FormId(target),
                            FormId(weapon),
                            kind,
                            slot,
                            part_av,
                            peer.character.feet,
                            peer.input.sneaking,
                            walkers,
                        );
                    }
                    let remaining = peer.action_points;
                    let outcome = if !accepted {
                        "rejected"
                    } else if hit {
                        "approved hit"
                    } else {
                        "approved miss"
                    };
                    println!(
                        "Co-op player {} V.A.T.S. attack {} {} (host AP {:.1})",
                        peer.player.0, attack_id, outcome, remaining
                    );
                    peer.last_attack_result =
                        Some((sequence, attack_id, shot_index, accepted, hit));
                    let _ = send(
                        socket,
                        &Packet::VatsAttackResult {
                            sequence,
                            attack_id,
                            shot_index,
                            accepted,
                            action_points: remaining,
                            hit,
                        },
                        address,
                    );
                }
            }
            _ => {}
        }
    }
}

fn roll_vats_hit(state: &mut world::scripting::GameState, hit_chance: f32) -> bool {
    hit_chance.is_finite()
        && (0.0..=1.0).contains(&hit_chance)
        && ((state.roll() % 1_000_000) as f32 / 1_000_000.0) < hit_chance
}

fn valid_vats_part(
    order: &esm::LoadOrder,
    state: &world::scripting::GameState,
    target: FormId,
    slot: i8,
    part_av: i32,
) -> bool {
    let body = world::body_parts::BodyPartData::of(order, target);
    let held = world::combat::weapon_in_hand(order, state, target);
    world::vats::parts_offered(body.as_ref(), held.as_ref())
        .iter()
        .any(|part| part.slot == slot && part.actor_value == part_av)
}

fn expected_vats_attack_cost(
    order: &esm::LoadOrder,
    state: &world::scripting::GameState,
    settings: &world::vats::Settings,
    target_down: bool,
    weapon_id: FormId,
    attack_kind: u8,
    reload: bool,
) -> Option<f32> {
    let weapon = if weapon_id == FormId(0) {
        None
    } else {
        Some(world::combat::Weapon::load(order, weapon_id)?)
    };

    let base_cost = match attack_kind {
        world::vats::kind::SPECIAL
        | world::vats::kind::UPPERCUT
        | world::vats::kind::CROSS
        | world::vats::kind::STOMP => {
            world::vats::specials(order, state, settings, weapon.as_ref(), target_down)
                .into_iter()
                .find(|special| special.kind == attack_kind)
                .map(|special| special.cost)?
        }
        kind if kind
            == world::vats::attack_kind(weapon.as_ref().map(|weapon| weapon.animation))
            && kind != world::vats::kind::NONE =>
        {
            world::vats::attack_cost(order, state, settings, weapon.as_ref())
        }
        _ => return None,
    };

    if reload
        && weapon
            .as_ref()
            .is_none_or(|weapon| weapon.is_melee() || weapon.ammo.is_empty())
    {
        return None;
    }
    let cost = base_cost
        + if reload {
            settings.cost(world::vats::kind::RELOAD)
        } else {
            0.0
        };
    (cost.is_finite() && cost > 0.0).then_some(cost)
}

fn apply_host_vats_shot(
    order: &esm::LoadOrder,
    scripts: &world::scripting::ScriptCache,
    state: &mut world::scripting::GameState,
    target: FormId,
    weapon_id: FormId,
    attack_kind: u8,
    slot: i8,
    part_av: i32,
    attacker_feet: [f32; 3],
    sneaking: bool,
    walkers: &Query<&ai::Walker>,
) {
    let Some(target_walker) = walkers.iter().find(|walker| walker.reference == target) else {
        return;
    };
    let weapon = if weapon_id.0 == 0 {
        None
    } else {
        let Some(weapon) = world::combat::Weapon::load(order, weapon_id) else {
            return;
        };
        Some(weapon)
    };
    let delta = [0, 1, 2].map(|axis| target_walker.position[axis] - attacker_feet[axis]);
    let distance = delta.iter().map(|value| value * value).sum::<f32>().sqrt();
    let old_vats = state.vats.clone();
    let old_sneaking = state.player_sneaking;
    state.player_sneaking = sneaking;
    world::vats::begin_attack(
        state,
        world::vats::AttackFacts {
            weapon: weapon.as_ref().map(|weapon| weapon.form_id),
            target,
            part_av,
            kind: attack_kind,
            hit: true,
            distance,
            paralyzing_palm: false,
        },
    );
    let part = u8::try_from(slot).ok();
    let result = if world::vats::is_melee_kind(attack_kind) {
        let blow = world::melee::Blow {
            power: false,
            special: world::vats::attack_group(attack_kind, sneaking)
                .map_or(world::melee::Special::None, |group| {
                    world::melee::special_of(weapon.as_ref(), group)
                }),
        };
        world::scripting::Runner::new(order, scripts, state).blow_at(
            world::dialogue::PLAYER_REF,
            target,
            weapon.as_ref(),
            part,
            blow,
        )
    } else {
        world::scripting::Runner::new(order, scripts, state).hit_at(
            world::dialogue::PLAYER_REF,
            target,
            weapon.as_ref(),
            part,
        )
    };
    let remaining = world::combat::health(order, state, target).unwrap_or(0.0);
    if let Some(hit) = result {
        println!(
            "Co-op host resolved V.A.T.S. damage to {target}: {:.1} ({remaining:.1} health remains)",
            hit.dealt
        );
    }
    state.player_sneaking = old_sneaking;
    state.vats = old_vats;
}

fn add_peer(
    peers: &mut HashMap<SocketAddr, Peer>,
    address: SocketAddr,
    start: [f32; 3],
    max_action_points: f32,
) -> Option<PlayerId> {
    if let Some(peer) = peers.get(&address) {
        return Some(peer.player);
    }
    if peers.len() >= MAX_PLAYERS - 1 {
        return None;
    }
    let id = (1..MAX_PLAYERS as u8)
        .map(PlayerId)
        .find(|candidate| peers.values().all(|peer| peer.player != *candidate))?;
    let mut feet = start;
    feet[0] += f32::from(id.0) * 48.0;
    peers.insert(
        address,
        Peer {
            player: id,
            character: Character::new(feet),
            spawn_feet: feet,
            reported_nonzero_input: false,
            reported_host_movement: false,
            input: PlayerInput {
                sequence: 0,
                axes: [0.0; 2],
                heading: 0.0,
                pitch: 0.0,
                view_aspect: 16.0 / 9.0,
                running: false,
                sneaking: false,
            },
            last_input_sequence: None,
            last_seen: Instant::now(),
            campaign: CampaignLedger::default(),
            last_world_update_revision: None,
            last_aim_sequence: None,
            action_points: max_action_points,
            max_action_points,
            last_vats_activity: None,
            last_attack_sequence: None,
            last_attack_result: None,
            vats_attack: None,
            last_world_keyframe_request: None,
            world_keyframe: None,
            last_dialogue_sequence: None,
            dialogue: None,
        },
    );
    Some(id)
}

fn validate_vats_target(
    order: &esm::LoadOrder,
    settings: &world::vats::Settings,
    state: &world::scripting::GameState,
    collision: &physics::Collider,
    walkers: &Query<&ai::Walker>,
    peer: &Peer,
    target: FormId,
) -> bool {
    let Some(walker) = walkers.iter().find(|walker| walker.reference == target) else {
        return false;
    };
    let feet = peer.character.feet;
    let position = walker.position;
    let distance = ((position[0] - feet[0]).powi(2)
        + (position[1] - feet[1]).powi(2)
        + (position[2] - feet[2]).powi(2))
    .sqrt();
    let radius = world::vats::bound_of(order, target).max(0.0) * 0.5;
    let eye = [feet[0], feet[1], feet[2] + cellview::EYE_HEIGHT];
    let aim_point = [position[0], position[1], position[2] + radius];
    let offset = [
        aim_point[0] - eye[0],
        aim_point[1] - eye[1],
        aim_point[2] - eye[2],
    ];
    let (sin_heading, cos_heading) = peer.input.heading.sin_cos();
    let (sin_pitch, cos_pitch) = peer.input.pitch.sin_cos();
    let forward = [
        sin_heading * cos_pitch,
        cos_heading * cos_pitch,
        sin_pitch,
    ];
    let right = [cos_heading, -sin_heading, 0.0];
    let up = [
        -sin_heading * sin_pitch,
        -cos_heading * sin_pitch,
        cos_pitch,
    ];
    let view_depth = offset[0] * forward[0] + offset[1] * forward[1] + offset[2] * forward[2];
    let screen_x = offset[0] * right[0] + offset[1] * right[1] + offset[2] * right[2];
    let screen_y = offset[0] * up[0] + offset[1] * up[1] + offset[2] * up[2];
    let half_horizontal_fov =
        cellview::horizontal_fov(cellview::GAME_FOV_DEGREES, peer.input.view_aspect) * 0.5;
    let half_vertical_fov = cellview::vertical_fov(cellview::GAME_FOV_DEGREES) * 0.5;
    let visible_depth = view_depth.max(0.0);
    let on_screen = view_depth + radius > 0.0
        && screen_x.abs() <= visible_depth * half_horizontal_fov.tan() + radius
        && screen_y.abs() <= visible_depth * half_vertical_fov.tan() + radius;
    let candidate = world::vats::Candidate {
        reference: target,
        alive: !state.dead.contains(&target),
        distance,
        line_of_sight: crate::fighting::clear_ray_between(collision, eye, aim_point),
        on_screen,
    };
    world::vats::eligible(order, settings, &candidate, state.player_world.is_none())
}

fn step_peers(
    peers: &mut HashMap<SocketAddr, Peer>,
    collider: &mut physics::Collider,
    speed_settings: &SpeedSettings,
    game: &GameFiles,
    state: &world::scripting::GameState,
    nav: Option<&ai::CellNav>,
    walkers: &Query<&ai::Walker>,
    host_position: [f32; 3],
) {
    let addresses: Vec<_> = peers.keys().copied().collect();
    for address in addresses {
        let Some(player_id) = peers.get(&address).map(|peer| peer.player) else {
            continue;
        };
        collider.set_people(world_people(
            walkers,
            state,
            peers,
            host_position,
            Some(player_id),
        ));
        let Some(peer) = peers.get_mut(&address) else {
            continue;
        };
        let speed = locomotion::speed(
            &game.0.order,
            state,
            speed_settings,
            world::dialogue::PLAYER_REF,
            peer.input.running,
            peer.input.sneaking,
        );
        let axes = peer.input.axes;
        let desired = [axes[0] * speed, axes[1] * speed];
        peer.character.update_controlled(
            collider,
            &CharacterShape::PLAYER,
            desired,
            None,
            locomotion::air_gain(AIR_CONTROL),
            NETWORK_TICK.as_secs_f32(),
        );
        let feet = peer.character.feet;
        let land = nav.and_then(|nav| nav.land_height(state.player_world, feet));
        let lifted = world::ground::kept_above_land(feet[2], land);
        if lifted != feet[2] {
            peer.character.feet[2] = lifted;
            peer.character.ground = lifted;
        }
        if !peer.reported_host_movement
            && squared_distance(peer.character.feet, peer.spawn_feet) >= 50.0f32.powi(2)
        {
            println!(
                "Co-op player {} moved under host simulation to ({:.1}, {:.1}, {:.1})",
                peer.player.0,
                peer.character.feet[0],
                peer.character.feet[1],
                peer.character.feet[2]
            );
            peer.reported_host_movement = true;
        }
    }
}

fn squared_distance(left: [f32; 3], right: [f32; 3]) -> f32 {
    (left[0] - right[0]).powi(2) + (left[1] - right[1]).powi(2) + (left[2] - right[2]).powi(2)
}

fn world_people(
    walkers: &Query<&ai::Walker>,
    state: &world::scripting::GameState,
    peers: &HashMap<SocketAddr, Peer>,
    host_position: [f32; 3],
    exclude: Option<PlayerId>,
) -> Vec<Person> {
    let mut people = walk::people(walkers, state);
    people.push(person(host_position));
    people.extend(
        peers
            .values()
            .filter(|peer| Some(peer.player) != exclude)
            .map(|peer| person(peer.character.feet)),
    );
    people
}

fn speaker_distance_squared(
    center: [f32; 3],
    dialogue: &DialogueSnapshot,
    walkers: &Query<&ai::Walker>,
) -> Option<f32> {
    let position = walkers
        .iter()
        .find(|walker| walker.reference == FormId(dialogue.speaker))?
        .position;
    Some(
        (position[0] - center[0]).powi(2)
            + (position[1] - center[1]).powi(2)
            + (position[2] - center[2]).powi(2),
    )
}

fn nearby_dialogue(
    center: [f32; 3],
    listener: PlayerId,
    host_dialogue: Option<&DialogueSnapshot>,
    peers: &HashMap<SocketAddr, Peer>,
    walkers: &Query<&ai::Walker>,
) -> Option<(PlayerId, DialogueSnapshot)> {
    let mut candidates = Vec::new();
    if listener != PlayerId(0) {
        if let Some(dialogue) = host_dialogue {
            if let Some(distance) = speaker_distance_squared(center, dialogue, walkers) {
                if distance <= DIALOGUE_LISTEN_RADIUS.powi(2) {
                    candidates.push((distance, PlayerId(0), dialogue));
                }
            }
        }
    }
    for peer in peers.values().filter(|peer| peer.player != listener) {
        if let Some(dialogue) = peer.dialogue.as_ref() {
            if let Some(distance) = speaker_distance_squared(center, dialogue, walkers) {
                if distance <= DIALOGUE_LISTEN_RADIUS.powi(2) {
                    candidates.push((distance, peer.player, dialogue));
                }
            }
        }
    }
    candidates
        .into_iter()
        .min_by(|left, right| left.0.total_cmp(&right.0))
        .map(|(_, player, dialogue)| (player, dialogue.clone()))
}

fn world_people_from_poses(
    walkers: &Query<&ai::Walker>,
    state: &world::scripting::GameState,
    poses: &HashMap<PlayerId, PlayerPose>,
    assigned: Option<PlayerId>,
) -> Vec<Person> {
    let mut people = walk::people(walkers, state);
    people.extend(
        poses
            .values()
            .filter(|pose| Some(pose.id) != assigned)
            .map(|pose| Person {
                feet: pose.position,
                radius: CharacterShape::PLAYER.radius,
                height: CharacterShape::PLAYER.height,
            }),
    );
    people
}

fn poll_client(
    socket: &UdpSocket,
    server: SocketAddr,
    scene_key: u64,
    order: &esm::LoadOrder,
    assigned: &mut Option<PlayerId>,
    last_tick: &mut Option<u32>,
    poses: &mut HashMap<PlayerId, PlayerPose>,
    last_world_revision: &mut Option<u32>,
    confirmed_dead: &mut HashSet<u32>,
    confirmed_destroyed: &mut HashSet<u32>,
    confirmed_health: &mut HashMap<u32, i32>,
    world_keyframe: &mut ClientWorldKeyframe,
    last_dialogue_sequence: &mut Option<u32>,
    listener_dialogue: &mut crate::dialogue::ListenerDialogue,
    campaign_events: &mut VecDeque<CampaignEvent>,
    world_update_events: &mut VecDeque<WorldEvent>,
    observed_client_dead: &mut HashSet<u32>,
    observed_client_destroyed: &mut HashSet<u32>,
    client_world_initialized: &mut bool,
    aim_target: Option<FormId>,
    last_aim_result_sequence: &mut Option<u32>,
    validated_aim: &mut Option<(FormId, Instant)>,
    vats_attack: &mut Option<VatsAttackRequest>,
    state: &mut world::scripting::GameState,
    now: Instant,
    incoming: &mut [u8; MAX_PACKET_BYTES],
) {
    loop {
        let (length, address) = match socket.recv_from(incoming) {
            Ok(result) => result,
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => break,
            Err(error) => {
                eprintln!("Co-op receive failed: {error}");
                break;
            }
        };
        if address != server {
            continue;
        }
        match Packet::decode(&incoming[..length]) {
            Ok(Packet::Welcome {
                player,
                scene_key: received_scene,
            }) if received_scene == scene_key => {
                if *assigned != Some(player) {
                    println!("Connected to co-op host as player {}", player.0);
                }
                *assigned = Some(player);
                if last_world_revision.is_none() {
                    request_world_keyframe(world_keyframe, now);
                }
            }
            Ok(Packet::Snapshot {
                tick,
                assigned_player,
                players,
                world_revision,
                campaign_revision,
                world_update_revision,
                world_events,
            }) if assigned.map_or(true, |id| id == assigned_player) => {
                *assigned = Some(assigned_player);
                if last_tick.map_or(true, |previous| sequence_is_newer(tick, previous)) {
                    *last_tick = Some(tick);
                    poses.clear();
                    poses.extend(players.into_iter().map(|pose| (pose.id, pose)));
                }
                world_update_events
                    .retain(|event| sequence_is_newer(event.revision, world_update_revision));
                if last_world_revision.is_none() {
                    request_world_keyframe(world_keyframe, now);
                } else if world_keyframe.pending_transfer_id.is_none()
                    && apply_world_events(
                        &world_events,
                        world_revision,
                        last_world_revision,
                        confirmed_dead,
                        confirmed_destroyed,
                        confirmed_health,
                    )
                {
                    request_world_keyframe(world_keyframe, now);
                }
                if last_world_revision.is_some() && world_keyframe.pending_transfer_id.is_none() {
                    sync_client_world(
                        order,
                        state,
                        confirmed_dead,
                        confirmed_destroyed,
                        confirmed_health,
                        world_update_events,
                    );
                    mark_client_world_observed(
                        state,
                        observed_client_dead,
                        observed_client_destroyed,
                        client_world_initialized,
                    );
                }
                campaign_events
                    .retain(|event| sequence_is_newer(event.revision, campaign_revision));
            }
            Ok(Packet::WorldKeyframeChunk {
                transfer_id,
                revision,
                index,
                count,
                entries,
            }) if assigned.is_some() => {
                if world_keyframe.completed_transfer_id == Some(transfer_id) {
                    let _ = send(socket, &Packet::WorldKeyframeAck { transfer_id }, server);
                } else if world_keyframe.pending_transfer_id == Some(transfer_id) {
                    if world_keyframe.assembly.as_ref().is_none_or(|assembly| {
                        assembly.transfer_id != transfer_id
                            || assembly.revision != revision
                            || assembly.chunks.len() != usize::from(count)
                    }) {
                        world_keyframe.assembly = Some(WorldKeyframeAssembly {
                            transfer_id,
                            revision,
                            received: 0,
                            chunks: vec![None; usize::from(count)],
                        });
                    }
                    if let Some(assembly) = world_keyframe.assembly.as_mut() {
                        let slot = &mut assembly.chunks[usize::from(index)];
                        if slot.is_none() {
                            *slot = Some(entries);
                            assembly.received += 1;
                        }
                        if assembly.received == assembly.chunks.len() {
                            let mut dead = HashSet::new();
                            let mut destroyed = HashSet::new();
                            let mut health = HashMap::new();
                            for chunk in assembly.chunks.iter().flatten() {
                                for entry in chunk {
                                    if entry.dead {
                                        dead.insert(entry.reference);
                                    }
                                    if entry.destroyed {
                                        destroyed.insert(entry.reference);
                                    }
                                    if let Some(value) = entry.health_milli {
                                        health.insert(entry.reference, value);
                                    }
                                }
                            }
                            *confirmed_dead = dead;
                            *confirmed_destroyed = destroyed;
                            *confirmed_health = health;
                            *last_world_revision = Some(assembly.revision);
                            world_keyframe.completed_transfer_id = Some(transfer_id);
                            world_keyframe.pending_transfer_id = None;
                            world_keyframe.assembly = None;
                            sync_client_world(
                                order,
                                state,
                                confirmed_dead,
                                confirmed_destroyed,
                                confirmed_health,
                                world_update_events,
                            );
                            mark_client_world_observed(
                                state,
                                observed_client_dead,
                                observed_client_destroyed,
                                client_world_initialized,
                            );
                            let _ = send(socket, &Packet::WorldKeyframeAck { transfer_id }, server);
                        }
                    }
                }
            }
            Ok(Packet::DialogueListen {
                player,
                sequence,
                dialogue,
            }) if last_dialogue_sequence
                .map_or(true, |previous| sequence_is_newer(sequence, previous)) =>
            {
                *last_dialogue_sequence = Some(sequence);
                listener_dialogue.0 = dialogue.map(|dialogue| (player, dialogue));
            }
            Ok(Packet::VatsAimResult {
                sequence,
                target,
                accepted,
            }) if aim_target == Some(FormId(target))
                && last_aim_result_sequence
                    .map_or(true, |previous| sequence_is_newer(sequence, previous)) =>
            {
                *last_aim_result_sequence = Some(sequence);
                *validated_aim = accepted.then_some((FormId(target), Instant::now()));
            }
            Ok(Packet::VatsAttackResult {
                sequence,
                attack_id,
                shot_index,
                accepted,
                action_points,
                hit,
            }) if vats_attack.is_some_and(|request| {
                request.sequence == sequence
                    && request.attack_id == attack_id
                    && request.shot_index == shot_index
            }) =>
            {
                println!(
                    "V.A.T.S. host {} attack {} (AP {:.1})",
                    if accepted { "approved" } else { "rejected" },
                    attack_id,
                    action_points
                );
                if !world::vats::set_action_points(order, state, action_points) {
                    eprintln!("Co-op host returned invalid action points: {action_points}");
                }
                if let Some(request) = vats_attack.as_mut() {
                    request.decision = Some(accepted);
                    request.hit = Some(hit);
                }
            }
            _ => {}
        }
    }
}

fn capture_world_changes(
    order: &esm::LoadOrder,
    walkers: &Query<&ai::Walker>,
    revision: &mut u32,
    journal: &mut VecDeque<WorldEvent>,
    known_dead: &mut HashSet<u32>,
    known_destroyed: &mut HashSet<u32>,
    known_health: &mut HashMap<u32, i32>,
    state: &world::scripting::GameState,
    max_events: usize,
    keep_latest: bool,
    capture_health: bool,
) {
    let dead: HashSet<u32> = state
        .dead
        .iter()
        .filter(|reference| **reference != world::dialogue::PLAYER_REF)
        .map(|reference| reference.0)
        .collect();
    let destroyed: HashSet<u32> = state
        .destroyed
        .iter()
        .map(|reference| reference.0)
        .collect();
    let health: HashMap<u32, i32> = if capture_health {
        walkers
            .iter()
            .filter(|walker| walker.reference != world::dialogue::PLAYER_REF)
            .filter_map(|walker| {
                actor_health_milli(order, state, walker.reference)
                    .map(|value| (walker.reference.0, value))
            })
            .collect()
    } else {
        HashMap::new()
    };
    let mut changes = Vec::new();
    changes.extend(
        ordered_difference(&dead, known_dead)
            .into_iter()
            .map(|reference| WorldChange::ActorDied { reference }),
    );
    changes.extend(
        ordered_difference(known_dead, &dead)
            .into_iter()
            .map(|reference| WorldChange::ActorRevived { reference }),
    );
    changes.extend(
        ordered_difference(&destroyed, known_destroyed)
            .into_iter()
            .map(|reference| WorldChange::ReferenceDestroyed { reference }),
    );
    changes.extend(
        ordered_difference(known_destroyed, &destroyed)
            .into_iter()
            .map(|reference| WorldChange::ReferenceRestored { reference }),
    );
    if capture_health {
        let mut changed_health: Vec<_> = health
            .iter()
            .filter(|(reference, value)| known_health.get(reference) != Some(*value))
            .map(|(&reference, &health_milli)| (reference, health_milli))
            .collect();
        changed_health.sort_unstable_by_key(|(reference, _)| *reference);
        changes.extend(changed_health.into_iter().map(|(reference, health_milli)| {
            WorldChange::ActorHealth {
                reference,
                health_milli,
            }
        }));
    }
    if !keep_latest && journal.len().saturating_add(changes.len()) > max_events {
        return;
    }
    for change in changes {
        *revision = revision.wrapping_add(1);
        journal.push_back(WorldEvent {
            revision: *revision,
            change,
        });
        if journal.len() > max_events {
            journal.pop_front();
        }
    }
    *known_dead = dead;
    *known_destroyed = destroyed;
    if capture_health {
        *known_health = health;
    }
}

fn actor_health_milli(
    order: &esm::LoadOrder,
    state: &world::scripting::GameState,
    reference: FormId,
) -> Option<i32> {
    let health = world::combat::health(order, state, reference)?;
    (health.is_finite() && health >= 0.0)
        .then(|| (health * 1000.0).round().min(f64::from(i32::MAX)) as i32)
}

fn apply_actor_health_milli(
    order: &esm::LoadOrder,
    state: &mut world::scripting::GameState,
    reference: u32,
    health_milli: i32,
) {
    let reference = FormId(reference);
    let Some(maximum) = world::combat::max_health(order, state, reference) else {
        return;
    };
    let current = (f64::from(health_milli) / 1000.0).min(maximum);
    let damage = (maximum - current).max(0.0);
    if damage == 0.0 {
        state.damage.remove(&reference);
    } else {
        state.damage.insert(reference, damage);
    }
}

fn ordered_difference(left: &HashSet<u32>, right: &HashSet<u32>) -> Vec<u32> {
    let mut difference: Vec<_> = left.difference(right).copied().collect();
    difference.sort_unstable();
    difference
}

fn capture_campaign_changes(
    revision: &mut u32,
    journal: &mut VecDeque<CampaignEvent>,
    known_stages: &mut HashMap<u32, u16>,
    known_stages_done: &mut HashSet<(u32, u16)>,
    known_objectives: &mut HashSet<(u32, i32)>,
    known_quest_status: &mut HashMap<u32, (bool, bool, bool)>,
    known_reputations: &mut HashMap<u32, (f32, f32)>,
    state: &world::scripting::GameState,
) {
    let stages: HashMap<u32, u16> = state
        .stages
        .iter()
        .filter(|(_, stage)| **stage != 0)
        .map(|(quest, stage)| (quest.0, *stage))
        .collect();
    let mut quest_ids: HashSet<u32> = known_stages.keys().copied().collect();
    quest_ids.extend(stages.keys().copied());
    let mut quest_ids: Vec<_> = quest_ids.into_iter().collect();
    quest_ids.sort_unstable();
    for quest in quest_ids {
        let old = known_stages.get(&quest).copied().unwrap_or_default();
        let new = stages.get(&quest).copied().unwrap_or_default();
        if old != new {
            push_campaign_event(
                revision,
                journal,
                CampaignChange::QuestStage { quest, stage: new },
            );
        }
    }
    *known_stages = stages;

    let stages_done: HashSet<(u32, u16)> = state
        .stages_done
        .iter()
        .map(|(quest, stage)| (quest.0, *stage))
        .collect();
    for (quest, stage) in ordered_set_difference(&stages_done, known_stages_done) {
        push_campaign_event(
            revision,
            journal,
            CampaignChange::QuestStageDone {
                quest,
                stage,
                done: true,
            },
        );
    }
    for (quest, stage) in ordered_set_difference(known_stages_done, &stages_done) {
        push_campaign_event(
            revision,
            journal,
            CampaignChange::QuestStageDone {
                quest,
                stage,
                done: false,
            },
        );
    }
    *known_stages_done = stages_done;

    let objectives: HashSet<(u32, i32)> = state
        .objectives
        .iter()
        .filter(|(_, completed)| **completed)
        .map(|((quest, objective), _)| (quest.0, *objective))
        .collect();
    for (quest, objective) in ordered_objective_difference(&objectives, known_objectives) {
        push_campaign_event(
            revision,
            journal,
            CampaignChange::QuestObjective {
                quest,
                objective,
                completed: true,
            },
        );
    }
    for (quest, objective) in ordered_objective_difference(known_objectives, &objectives) {
        push_campaign_event(
            revision,
            journal,
            CampaignChange::QuestObjective {
                quest,
                objective,
                completed: false,
            },
        );
    }
    *known_objectives = objectives;

    let mut status_ids: HashSet<u32> = known_quest_status.keys().copied().collect();
    status_ids.extend(state.running.iter().map(|quest| quest.0));
    status_ids.extend(state.completed.iter().map(|quest| quest.0));
    status_ids.extend(state.failed.iter().map(|quest| quest.0));
    let mut status_ids: Vec<_> = status_ids.into_iter().collect();
    status_ids.sort_unstable();
    let mut quest_status = HashMap::new();
    for quest in status_ids {
        let status = (
            state.running.contains(&esm::FormId(quest)),
            state.completed.contains(&esm::FormId(quest)),
            state.failed.contains(&esm::FormId(quest)),
        );
        let previous = known_quest_status.get(&quest).copied().unwrap_or_default();
        if previous != status {
            push_campaign_event(
                revision,
                journal,
                CampaignChange::QuestStatus {
                    quest,
                    running: status.0,
                    completed: status.1,
                    failed: status.2,
                },
            );
        }
        if status != (false, false, false) {
            quest_status.insert(quest, status);
        }
    }
    *known_quest_status = quest_status;

    let reputations: HashMap<u32, (f32, f32)> = state
        .reputations
        .iter()
        .map(|(faction, (fame, infamy))| {
            (
                faction.0,
                (
                    if fame.is_finite() { *fame } else { 0.0 },
                    if infamy.is_finite() { *infamy } else { 0.0 },
                ),
            )
        })
        .filter(|(_, values)| *values != (0.0, 0.0))
        .collect();
    let mut faction_ids: HashSet<u32> = known_reputations.keys().copied().collect();
    faction_ids.extend(reputations.keys().copied());
    let mut faction_ids: Vec<_> = faction_ids.into_iter().collect();
    faction_ids.sort_unstable();
    for faction in faction_ids {
        let old = known_reputations.get(&faction).copied().unwrap_or_default();
        let new = reputations.get(&faction).copied().unwrap_or_default();
        if old != new {
            push_campaign_event(
                revision,
                journal,
                CampaignChange::Reputation {
                    reputation: faction,
                    fame: new.0,
                    infamy: new.1,
                },
            );
        }
    }
    *known_reputations = reputations;
}

fn ordered_set_difference(
    left: &HashSet<(u32, u16)>,
    right: &HashSet<(u32, u16)>,
) -> Vec<(u32, u16)> {
    let mut difference: Vec<_> = left.difference(right).copied().collect();
    difference.sort_unstable();
    difference
}

fn ordered_objective_difference(
    left: &HashSet<(u32, i32)>,
    right: &HashSet<(u32, i32)>,
) -> Vec<(u32, i32)> {
    let mut difference: Vec<_> = left.difference(right).copied().collect();
    difference.sort_unstable();
    difference
}

fn push_campaign_event(
    revision: &mut u32,
    journal: &mut VecDeque<CampaignEvent>,
    change: CampaignChange,
) {
    *revision = revision.wrapping_add(1);
    journal.push_back(CampaignEvent {
        revision: *revision,
        change,
    });
    if journal.len() > MAX_PENDING_CAMPAIGN_EVENTS {
        journal.pop_front();
    }
}

#[derive(Default)]
struct CampaignQuestSchema {
    stages: HashSet<u16>,
    objectives: HashSet<i32>,
}

fn campaign_quest_schema<'a>(
    order: &esm::LoadOrder,
    quest_id: u32,
    schemas: &'a mut HashMap<u32, Option<CampaignQuestSchema>>,
) -> Option<&'a CampaignQuestSchema> {
    schemas
        .entry(quest_id)
        .or_insert_with(|| {
            world::quest::Quest::load(order, FormId(quest_id)).map(|quest| CampaignQuestSchema {
                stages: quest
                    .stages
                    .iter()
                    .map(|stage| stage.index as u16)
                    .collect(),
                objectives: quest
                    .objectives
                    .iter()
                    .map(|objective| objective.index)
                    .collect(),
            })
        })
        .as_ref()
}

fn campaign_change_is_valid(
    order: &esm::LoadOrder,
    schemas: &mut HashMap<u32, Option<CampaignQuestSchema>>,
    change: CampaignChange,
) -> bool {
    match change {
        CampaignChange::QuestStage { quest, stage } => campaign_quest_schema(order, quest, schemas)
            .is_some_and(|schema| stage == 0 || schema.stages.contains(&stage)),
        CampaignChange::QuestStageDone { quest, stage, .. } => {
            campaign_quest_schema(order, quest, schemas)
                .is_some_and(|schema| schema.stages.contains(&stage))
        }
        CampaignChange::QuestObjective {
            quest, objective, ..
        } => campaign_quest_schema(order, quest, schemas)
            .is_some_and(|schema| schema.objectives.contains(&objective)),
        CampaignChange::QuestStatus { quest, .. } => {
            campaign_quest_schema(order, quest, schemas).is_some()
        }
        CampaignChange::Reputation { reputation, .. } => order
            .get(FormId(reputation))
            .is_some_and(|record| record.entry.header.kind == esm::FourCC::new(b"REPU")),
    }
}

fn apply_campaign_events(
    ledger: &mut CampaignLedger,
    events: &[CampaignEvent],
    order: &esm::LoadOrder,
    schemas: &mut HashMap<u32, Option<CampaignQuestSchema>>,
) {
    for event in events {
        if ledger
            .last_revision
            .is_some_and(|previous| !sequence_is_newer(event.revision, previous))
        {
            continue;
        }
        let expected = ledger.last_revision.unwrap_or_default().wrapping_add(1);
        if event.revision != expected {
            eprintln!(
                "Co-op campaign event gap: expected revision {expected}, received {}",
                event.revision
            );
            break;
        }
        if !campaign_change_is_valid(order, schemas, event.change) {
            eprintln!(
                "Rejected invalid co-op campaign event at revision {}",
                event.revision
            );
            // Consume invalid but contiguous events so one malformed report
            // cannot block every later event in this peer's campaign stream.
            ledger.last_revision = Some(event.revision);
            continue;
        }
        match event.change {
            CampaignChange::QuestStage { quest, stage } => {
                if stage == 0 {
                    ledger.stages.remove(&quest);
                } else {
                    ledger.stages.insert(quest, stage);
                }
            }
            CampaignChange::QuestStageDone { quest, stage, done } => {
                if done {
                    ledger.stages_done.insert((quest, stage));
                } else {
                    ledger.stages_done.remove(&(quest, stage));
                }
            }
            CampaignChange::QuestObjective {
                quest,
                objective,
                completed,
            } => {
                if completed {
                    ledger.objectives.insert((quest, objective));
                } else {
                    ledger.objectives.remove(&(quest, objective));
                }
            }
            CampaignChange::QuestStatus {
                quest,
                running,
                completed,
                failed,
            } => {
                if running || completed || failed {
                    ledger
                        .quest_status
                        .insert(quest, (running, completed, failed));
                } else {
                    ledger.quest_status.remove(&quest);
                }
            }
            CampaignChange::Reputation {
                reputation,
                fame,
                infamy,
            } => {
                if fame == 0.0 && infamy == 0.0 {
                    ledger.reputations.remove(&reputation);
                } else {
                    ledger.reputations.insert(reputation, (fame, infamy));
                }
            }
        }
        ledger.last_revision = Some(event.revision);
    }
}

fn apply_client_world_events(
    last_revision: &mut Option<u32>,
    events: &[WorldEvent],
    order: &esm::LoadOrder,
    state: &mut world::scripting::GameState,
    walkers: &Query<&ai::Walker>,
    exterior: Option<&crate::exterior::Exterior>,
    loaded_references: &mut Option<HashSet<u32>>,
    world_revision: &mut u32,
    world_events: &mut VecDeque<WorldEvent>,
    observed_dead: &mut HashSet<u32>,
    observed_destroyed: &mut HashSet<u32>,
) {
    for event in events {
        if last_revision.is_some_and(|previous| !sequence_is_newer(event.revision, previous)) {
            continue;
        }
        let expected = last_revision.unwrap_or_default().wrapping_add(1);
        if event.revision != expected {
            eprintln!(
                "Co-op guest world update gap: expected revision {expected}, received {}",
                event.revision
            );
            break;
        }
        if !world_change_is_valid(
            event.change,
            order,
            walkers,
            state,
            exterior,
            loaded_references,
        ) {
            eprintln!(
                "Rejected invalid co-op guest world update at revision {}",
                event.revision
            );
            *last_revision = Some(event.revision);
            continue;
        }
        let changed = apply_world_change(order, state, event.change);
        match event.change {
            WorldChange::ActorDied { reference } | WorldChange::ActorRevived { reference } => {
                if state.dead.contains(&FormId(reference)) {
                    observed_dead.insert(reference);
                } else {
                    observed_dead.remove(&reference);
                }
            }
            WorldChange::ReferenceDestroyed { reference }
            | WorldChange::ReferenceRestored { reference } => {
                if state.destroyed.contains(&FormId(reference)) {
                    observed_destroyed.insert(reference);
                } else {
                    observed_destroyed.remove(&reference);
                }
            }
            WorldChange::ActorHealth { .. } => {}
        }
        if changed {
            *world_revision = world_revision.wrapping_add(1);
            world_events.push_back(WorldEvent {
                revision: *world_revision,
                change: event.change,
            });
            if world_events.len() > MAX_WORLD_EVENTS {
                world_events.pop_front();
            }
        }
        *last_revision = Some(event.revision);
    }
}

fn world_change_is_valid(
    change: WorldChange,
    order: &esm::LoadOrder,
    walkers: &Query<&ai::Walker>,
    state: &world::scripting::GameState,
    exterior: Option<&crate::exterior::Exterior>,
    loaded_references: &mut Option<HashSet<u32>>,
) -> bool {
    let (reference, actor) = match change {
        WorldChange::ActorDied { reference } | WorldChange::ActorRevived { reference } => {
            (reference, true)
        }
        WorldChange::ActorHealth { .. } => return false,
        WorldChange::ReferenceDestroyed { reference }
        | WorldChange::ReferenceRestored { reference } => (reference, false),
    };
    if reference == 0 || reference == world::dialogue::PLAYER_REF.0 {
        return false;
    }
    let exists = if actor {
        walkers
            .iter()
            .any(|walker| walker.reference == FormId(reference))
    } else {
        let loaded_references = loaded_references
            .get_or_insert_with(|| loaded_world_references(order, state, exterior));
        order.get(FormId(reference)).is_some_and(|record| {
            record.entry.header.kind == esm::sig::REFR && loaded_references.contains(&reference)
        })
    };
    if !exists {
        return false;
    }
    // A guest report can acknowledge a host mutation that has not yet reached
    // its snapshot, but it cannot create, undo, or set a shared-world change.
    match change {
        WorldChange::ActorDied { .. } => state.dead.contains(&FormId(reference)),
        WorldChange::ActorRevived { .. } => !state.dead.contains(&FormId(reference)),
        WorldChange::ReferenceDestroyed { .. } => state.destroyed.contains(&FormId(reference)),
        WorldChange::ReferenceRestored { .. } => !state.destroyed.contains(&FormId(reference)),
        WorldChange::ActorHealth { .. } => false,
    }
}

fn loaded_world_references(
    order: &esm::LoadOrder,
    state: &world::scripting::GameState,
    exterior: Option<&crate::exterior::Exterior>,
) -> HashSet<u32> {
    let mut references = HashSet::new();
    if state.player_world.is_some() {
        if let Some(exterior) = exterior {
            for square in exterior.loaded_squares() {
                if let Some(cell) = exterior.grid.cell_at(square) {
                    references.extend(
                        order
                            .references_in_cell(cell)
                            .into_iter()
                            .map(|record| record.form_id.0),
                    );
                }
                references.extend(
                    exterior
                        .grid
                        .persistent_in(square)
                        .iter()
                        .map(|reference| reference.0),
                );
            }
        }
    } else if let Some(cell) = state.player_cell {
        references.extend(
            order
                .references_in_cell(cell)
                .into_iter()
                .map(|record| record.form_id.0),
        );
    }
    references
}

fn apply_world_change(
    order: &esm::LoadOrder,
    state: &mut world::scripting::GameState,
    change: WorldChange,
) -> bool {
    match change {
        WorldChange::ActorDied { reference } => state.dead.insert(FormId(reference)),
        WorldChange::ActorRevived { reference } => state.dead.remove(&FormId(reference)),
        WorldChange::ReferenceDestroyed { reference } => state.destroyed.insert(FormId(reference)),
        WorldChange::ReferenceRestored { reference } => state.destroyed.remove(&FormId(reference)),
        WorldChange::ActorHealth {
            reference,
            health_milli,
        } => {
            let before = actor_health_milli(order, state, FormId(reference));
            apply_actor_health_milli(order, state, reference, health_milli);
            before != actor_health_milli(order, state, FormId(reference))
        }
    }
}

fn request_world_keyframe(sync: &mut ClientWorldKeyframe, now: Instant) {
    if sync.pending_transfer_id.is_some() {
        return;
    }
    sync.next_transfer_id = sync.next_transfer_id.wrapping_add(1);
    sync.pending_transfer_id = Some(sync.next_transfer_id);
    sync.next_request = now;
    sync.assembly = None;
}

fn apply_world_events(
    events: &[WorldEvent],
    world_revision: u32,
    last_revision: &mut Option<u32>,
    confirmed_dead: &mut HashSet<u32>,
    confirmed_destroyed: &mut HashSet<u32>,
    confirmed_health: &mut HashMap<u32, i32>,
) -> bool {
    let Some(mut current_revision) = *last_revision else {
        return true;
    };
    let mut gap = false;
    for event in events {
        if sequence_is_newer(event.revision, current_revision) {
            let expected = current_revision.wrapping_add(1);
            if event.revision != expected {
                eprintln!(
                    "Co-op world event gap: expected revision {expected}, received {}",
                    event.revision
                );
                gap = true;
                break;
            }
            let reference = esm::FormId(match event.change {
                WorldChange::ActorDied { reference }
                | WorldChange::ActorRevived { reference }
                | WorldChange::ReferenceDestroyed { reference }
                | WorldChange::ReferenceRestored { reference }
                | WorldChange::ActorHealth { reference, .. } => reference,
            });
            match event.change {
                WorldChange::ActorDied { .. } => {
                    confirmed_dead.insert(reference.0);
                }
                WorldChange::ActorRevived { .. } => {
                    confirmed_dead.remove(&reference.0);
                }
                WorldChange::ReferenceDestroyed { .. } => {
                    confirmed_destroyed.insert(reference.0);
                }
                WorldChange::ReferenceRestored { .. } => {
                    confirmed_destroyed.remove(&reference.0);
                }
                WorldChange::ActorHealth { health_milli, .. } => {
                    confirmed_health.insert(reference.0, health_milli);
                }
            }
            current_revision = event.revision;
            *last_revision = Some(current_revision);
        }
    }
    if sequence_is_newer(world_revision, current_revision) {
        eprintln!(
            "Co-op world snapshot gap: latest revision is {world_revision}, applied through {current_revision}"
        );
        gap = true;
    }
    gap
}

fn sync_client_world(
    order: &esm::LoadOrder,
    state: &mut world::scripting::GameState,
    confirmed_dead: &HashSet<u32>,
    confirmed_destroyed: &HashSet<u32>,
    confirmed_health: &HashMap<u32, i32>,
    pending: &VecDeque<WorldEvent>,
) {
    sync_confirmed_world(state, confirmed_dead, confirmed_destroyed);
    for (&reference, &health) in confirmed_health {
        apply_actor_health_milli(order, state, reference, health);
    }
    for event in pending {
        apply_world_change(order, state, event.change);
    }
}

fn mark_client_world_observed(
    state: &world::scripting::GameState,
    observed_dead: &mut HashSet<u32>,
    observed_destroyed: &mut HashSet<u32>,
    initialized: &mut bool,
) {
    *observed_dead = state
        .dead
        .iter()
        .filter(|reference| **reference != world::dialogue::PLAYER_REF)
        .map(|reference| reference.0)
        .collect();
    *observed_destroyed = state
        .destroyed
        .iter()
        .map(|reference| reference.0)
        .collect();
    *initialized = true;
}

fn sync_confirmed_world(
    state: &mut world::scripting::GameState,
    confirmed_dead: &HashSet<u32>,
    confirmed_destroyed: &HashSet<u32>,
) {
    // Keep client-side prediction from committing shared flags. The host is
    // the source of truth; PLAYER_REF remains this client's personal actor.
    let local_player_dead = state.dead.contains(&world::dialogue::PLAYER_REF);
    state.dead = confirmed_dead.iter().copied().map(esm::FormId).collect();
    if local_player_dead {
        state.dead.insert(world::dialogue::PLAYER_REF);
    }
    state.destroyed = confirmed_destroyed
        .iter()
        .copied()
        .map(esm::FormId)
        .collect();
}

fn reconcile_local(player: &mut Player, authority: PlayerPose, dt: f32) {
    let blend = (1.0 - (-12.0 * dt).exp()).clamp(0.0, 1.0);
    for (local, target) in player
        .character
        .feet
        .iter_mut()
        .zip(authority.position.into_iter())
    {
        *local += (target - *local) * blend;
    }
}

fn update_avatars(
    commands: &mut Commands,
    meshes: &mut Assets<Mesh>,
    materials: &mut Assets<StandardMaterial>,
    avatars: &mut Query<(Entity, &RemoteAvatar, &mut Transform)>,
    poses: &HashMap<PlayerId, PlayerPose>,
    assigned_player: PlayerId,
    dt: f32,
) {
    let visible: HashMap<_, _> = poses
        .iter()
        .filter(|(id, _)| **id != assigned_player)
        .map(|(&id, &pose)| (id, pose))
        .collect();
    let blend = (1.0 - (-14.0 * dt).exp()).clamp(0.0, 1.0);
    let mut present = Vec::new();
    for (entity, avatar, mut transform) in avatars.iter_mut() {
        present.push((avatar.0, entity));
        if let Some(pose) = visible.get(&avatar.0) {
            let target = Vec3::from(space::point([
                pose.position[0],
                pose.position[1],
                pose.position[2] + CharacterShape::PLAYER.height * 0.5,
            ]));
            transform.translation = transform.translation.lerp(target, blend);
            transform.rotation = transform.rotation.slerp(
                Quat::from_rotation_y(space::heading_to_yaw(pose.heading)),
                blend,
            );
        }
    }
    for (id, entity) in &present {
        if !visible.contains_key(id) {
            commands.entity(*entity).despawn();
        }
    }
    for (&id, pose) in &visible {
        if present.iter().any(|(existing, _)| *existing == id) {
            continue;
        }
        let target = Vec3::from(space::point([
            pose.position[0],
            pose.position[1],
            pose.position[2] + CharacterShape::PLAYER.height * 0.5,
        ]));
        let hue = f32::from(id.0) * 0.21;
        let color = Color::hsla(hue * 360.0, 0.8, 0.62, 1.0);
        commands.spawn((
            Mesh3d(meshes.add(Capsule3d::new(0.28, 1.2))),
            MeshMaterial3d(materials.add(StandardMaterial {
                base_color: color,
                unlit: true,
                ..default()
            })),
            Transform::from_translation(target)
                .with_rotation(Quat::from_rotation_y(space::heading_to_yaw(pose.heading))),
            Name::new(format!("Co-op player {}", id.0)),
            RemoteAvatar(id),
        ));
    }
}

fn send(socket: &UdpSocket, packet: &Packet, address: SocketAddr) -> io::Result<()> {
    let bytes = packet
        .encode()
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
    socket.send_to(&bytes, address).map(|_| ())
}
