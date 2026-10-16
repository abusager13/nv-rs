//! Local host-authoritative player movement for the co-op experiment.
//!
//! This viewer slice is limited to peers in the same loaded cell. The host
//! simulates guest character controllers, broadcasts their poses, and owns
//! shared death/destruction state; guests predict movement locally and request
//! a complete world-flag keyframe when they join or detect a replay gap.

use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};
use std::io;
use std::net::{SocketAddr, UdpSocket};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use bevy::ecs::system::SystemParam;
use bevy::prelude::*;
use cellview::space;
use coop_sync_experiment::{
    sequence_is_newer, ActorValueEntry, CampaignChange, CampaignEvent, DialogueFactKind,
    DialogueSnapshot, InventoryEntry, Packet, PerkEntry, PlayerId, PlayerInput, PlayerPose,
    WorldChange, WorldEvent, WorldStateEntry, MAX_CAMPAIGN_EVENTS, MAX_PACKET_BYTES, MAX_PLAYERS,
    MAX_PLAYER_INVENTORY_ENTRIES, MAX_PLAYER_PROFILE_PERKS, MAX_VIEW_ASPECT, MAX_WORLD_EVENTS,
    MAX_WORLD_KEYFRAME_ENTRIES, MAX_WORLD_UPDATE_EVENTS, MIN_VIEW_ASPECT,
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
const DIALOGUE_STATE_TIMEOUT: Duration = Duration::from_millis(750);
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
    talkers: Res<'w, crate::dialogue::Talkers>,
    cell_scripts: Res<'w, crate::scripts::CellScripts>,
    rigs: Query<'w, 's, (&'static ai::Walker, &'static crate::actors::ActorRig)>,
}

#[derive(Resource)]
pub struct Session {
    endpoint: Endpoint,
    scene_key: u64,
    private_quests: HashSet<FormId>,
    private_globals: HashSet<FormId>,
    tick: u32,
    accumulator: f32,
    next_send: Instant,
    next_hello: Instant,
    input_sequence: u32,
    dialogue_sequence: u32,
    last_dialogue_sequence: Option<u32>,
    dialogue_choice_sequence: u32,
    dialogue_choices: VecDeque<PendingDialogueChoice>,
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
    confirmed_killers: HashMap<u32, u8>,
    world_keyframe: ClientWorldKeyframe,
    campaign_revision: u32,
    campaign_events: VecDeque<CampaignEvent>,
    shared_campaign_revision: u32,
    shared_campaign_events: VecDeque<CampaignEvent>,
    last_shared_campaign_revision: Option<u32>,
    published_host_campaign_revision: u32,
    shared_campaign: CampaignLedger,
    observed_stages: HashMap<u32, u16>,
    observed_stages_done: HashSet<(u32, u16)>,
    observed_objectives: HashSet<(u32, i32)>,
    observed_quest_status: HashMap<u32, (bool, bool, bool)>,
    observed_quest_variables: HashMap<(u32, u16), f32>,
    observed_globals: HashMap<u32, f32>,
    observed_reputations: HashMap<u32, (f32, f32)>,
    observed_dialogue_topics: HashSet<u32>,
    observed_dialogue_said: HashSet<u32>,
    observed_dialogue_talked_to: HashSet<u32>,
    observed_player_female: Option<bool>,
    player_sex_observed: bool,
    observed_player_items: HashMap<u32, u32>,
    player_items_observed: bool,
    observed_player_misc_stats: HashMap<u8, u32>,
    player_misc_stats_observed: bool,
    aim_sequence: u32,
    aim_target: Option<FormId>,
    last_aim_result_sequence: Option<u32>,
    validated_aim: Option<(FormId, Instant)>,
    next_aim_request: Instant,
    vats_attack_sequence: u32,
    vats_attack: Option<VatsAttackRequest>,
    player_attack_sequence: u32,
    player_attacks: VecDeque<PlayerAttackRequest>,
    inventory_initialized: bool,
    inventory_revision: u32,
    inventory_ack_revision: Option<u32>,
    inventory_entries: Vec<InventoryEntry>,
    profile_initialized: bool,
    profile_revision: u32,
    profile_ack_revision: Option<u32>,
    profile_level: u16,
    profile_actor_values: Vec<ActorValueEntry>,
    profile_perks: Vec<PerkEntry>,
}

#[derive(Clone, Copy)]
struct VatsAttackRequest {
    sequence: u32,
    attack_id: u64,
    shot_index: u16,
    target: FormId,
    weapon: Option<FormId>,
    inventory_revision: u32,
    profile_revision: u32,
    ammo: Option<FormId>,
    kind: u8,
    reload: bool,
    slot: i8,
    part_av: i32,
    action_points: f32,
    decision: Option<bool>,
    hit: Option<bool>,
}

#[derive(Clone, Copy)]
struct PlayerAttackRequest {
    sequence: u32,
    weapon: Option<FormId>,
    ammo: Option<FormId>,
    ammo_use: u32,
    attack_group: u8,
    power: bool,
}

#[derive(Clone)]
struct PendingDialogueChoice {
    sequence: u32,
    offer: DialogueSnapshot,
    source: Option<u32>,
    topic: u32,
    info: u32,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum PlayerAttackStatus {
    Local,
    Queued,
    Full,
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

    pub(crate) fn is_host(&self) -> bool {
        matches!(
            self.0.as_ref().map(|session| &session.endpoint),
            Some(Endpoint::Host { .. })
        )
    }

    pub(crate) fn remote_player_positions(&self) -> Vec<[f32; 3]> {
        let Some(session) = self.0.as_ref() else {
            return Vec::new();
        };
        match &session.endpoint {
            Endpoint::Host { peers, .. } => {
                peers.values().map(|peer| peer.character.feet).collect()
            }
            Endpoint::Client { .. } => session
                .poses
                .values()
                .filter(|pose| Some(pose.id) != session.assigned_player)
                .map(|pose| pose.position)
                .collect(),
        }
    }

    pub(crate) fn host_owns_damage(&self) -> bool {
        !matches!(
            self.0.as_ref().map(|session| &session.endpoint),
            Some(Endpoint::Client { .. })
        )
    }

    /// Queue a player attack for host resolution. A client never applies
    /// damage locally when its request is queued or the bounded queue is full.
    pub(crate) fn request_player_attack(
        &mut self,
        weapon: Option<FormId>,
        ammo: Option<FormId>,
        ammo_use: u32,
        attack_group: u8,
        power: bool,
    ) -> PlayerAttackStatus {
        const MAX_PENDING_ATTACKS: usize = 16;
        let Some(session) = self.0.as_mut() else {
            return PlayerAttackStatus::Local;
        };
        if !matches!(session.endpoint, Endpoint::Client { .. }) {
            return PlayerAttackStatus::Local;
        }
        if session.player_attacks.len() >= MAX_PENDING_ATTACKS {
            eprintln!("Co-op attack queue is full; the attack was ignored.");
            return PlayerAttackStatus::Full;
        }
        session.player_attack_sequence = session.player_attack_sequence.wrapping_add(1);
        session.player_attacks.push_back(PlayerAttackRequest {
            sequence: session.player_attack_sequence,
            weapon,
            ammo,
            ammo_use,
            attack_group,
            power,
        });
        PlayerAttackStatus::Queued
    }

    pub(crate) fn request_dialogue_choice(
        &mut self,
        offer: DialogueSnapshot,
        source: Option<u32>,
        topic: u32,
        info: u32,
    ) -> bool {
        const MAX_PENDING_CHOICES: usize = 16;
        let Some(session) = self.0.as_mut() else {
            return false;
        };
        if !matches!(session.endpoint, Endpoint::Client { .. })
            || offer.line.is_some()
            || offer.topics.is_empty()
            || source == Some(0)
            || !offer
                .topics
                .iter()
                .any(|choice| choice.topic == topic && choice.info == info)
            || session.dialogue_choices.len() >= MAX_PENDING_CHOICES
        {
            return false;
        }
        session.dialogue_choice_sequence = session.dialogue_choice_sequence.wrapping_add(1);
        session.dialogue_choices.push_back(PendingDialogueChoice {
            sequence: session.dialogue_choice_sequence,
            offer,
            source,
            topic,
            info,
        });
        true
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
        ammo: Option<FormId>,
        kind: u8,
        reload: bool,
        slot: i8,
        part_av: i32,
        action_points: f32,
    ) -> VatsAttackApproval {
        if !action_points.is_finite() || action_points <= 0.0 {
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
                    inventory_revision: session.inventory_revision,
                    profile_revision: session.profile_revision,
                    ammo,
                    kind,
                    reload,
                    slot,
                    part_av,
                    action_points,
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
    shared_campaign_revision: u32,
    last_world_update_revision: Option<u32>,
    last_aim_sequence: Option<u32>,
    action_points: f32,
    max_action_points: f32,
    last_vats_activity: Option<Instant>,
    last_attack_sequence: Option<u32>,
    last_attack_result: Option<(u32, u64, u16, bool, bool)>,
    last_player_attack_sequence: Option<u32>,
    last_player_attack_result: Option<(u32, bool)>,
    last_player_attack_at: Option<Instant>,
    attack_cache: crate::combat::PlayerAttack,
    inventory_revision: Option<u32>,
    inventory: HashMap<FormId, u32>,
    profile_revision: Option<u32>,
    profile: Option<CombatProfile>,
    vats_attack: Option<HostVatsAttack>,
    last_world_keyframe_request: Option<u32>,
    world_keyframe: Option<HostWorldKeyframe>,
    last_dialogue_sequence: Option<u32>,
    last_dialogue_choice_sequence: Option<u32>,
    last_dialogue_choice_result: Option<(u32, bool)>,
    dialogue_updated_at: Option<Instant>,
    dialogue: Option<DialogueSnapshot>,
    host_dialogue_line: Option<(u32, u32)>,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct CombatProfile {
    level: u16,
    actor_values: Vec<ActorValueEntry>,
    perks: Vec<PerkEntry>,
}

pub(crate) fn combat_profile_snapshot(
    order: &esm::LoadOrder,
    state: &world::scripting::GameState,
) -> Option<CombatProfile> {
    if state.perks.len() > MAX_PLAYER_PROFILE_PERKS {
        return None;
    }
    let facts = world::scripting::Facts {
        order,
        state,
        speaker: None,
    };
    let mut actor_values = Vec::new();
    for actor_value in 0..coop_sync_experiment::MAX_PLAYER_PROFILE_ACTOR_VALUES {
        let actor_value = u16::try_from(actor_value).ok()?;
        let (Some(current), Some(permanent)) = (
            facts.current_actor_value(world::dialogue::PLAYER_REF, actor_value),
            facts.permanent_actor_value(world::dialogue::PLAYER_REF, actor_value),
        ) else {
            continue;
        };
        let (current, permanent) = (current as f32, permanent as f32);
        if current.is_finite() && permanent.is_finite() {
            actor_values.push(ActorValueEntry {
                actor_value: u8::try_from(actor_value).ok()?,
                current,
                permanent,
            });
        }
    }
    let mut perks: Vec<_> = state.perks.iter().copied().collect();
    perks.sort_unstable();
    let perks = perks
        .into_iter()
        .map(|form| PerkEntry {
            form: form.0,
            rank: world::perks::rank(state, form),
        })
        .collect();
    Some(CombatProfile {
        level: state.player_level,
        actor_values,
        perks,
    })
}

/// Run one host calculation with the peer's combat actor values and perks
/// temporarily installed in the legacy single-player rules state.
pub(crate) fn with_combat_profile<T>(
    order: &esm::LoadOrder,
    state: &mut world::scripting::GameState,
    profile: &CombatProfile,
    operation: impl FnOnce(&mut world::scripting::GameState) -> T,
) -> T {
    let player = world::dialogue::PLAYER_REF;
    let saved_level = state.player_level;
    let saved_level_up_pending = state.level_up_pending;
    let saved_perks = std::mem::take(&mut state.perks);
    let saved_perk_ranks = std::mem::take(&mut state.perk_ranks);
    let saved_actor_values: Vec<_> = state
        .actor_values
        .iter()
        .filter_map(|(&(who, value), &number)| (who == player).then_some((value, number)))
        .collect();
    let saved_value_damage: Vec<_> = state
        .value_damage
        .iter()
        .filter_map(|(&(who, value), &number)| (who == player).then_some((value, number)))
        .collect();
    let saved_health_damage = state.damage.remove(&player);
    state.actor_values.retain(|(who, _), _| *who != player);
    state.value_damage.retain(|(who, _), _| *who != player);
    state.player_level = profile.level.max(1);
    state.perks = profile.perks.iter().map(|perk| FormId(perk.form)).collect();
    state.perk_ranks = profile
        .perks
        .iter()
        .map(|perk| (FormId(perk.form), perk.rank))
        .collect();

    for entry in &profile.actor_values {
        let av = u16::from(entry.actor_value);
        if av == world::combat::av::HEALTH {
            continue;
        }
        let permanent_modifier = world::magic::modifier(state, player, av, true);
        let current_modifier = world::magic::modifier(state, player, av, false);
        let base = f64::from(entry.permanent) - permanent_modifier;
        state.actor_values.insert((player, av), base);
        let damage = if world::magic::COUNTERS.contains(&av) {
            f64::from(entry.current) - current_modifier
        } else {
            f64::from(entry.permanent) + current_modifier
                - permanent_modifier
                - f64::from(entry.current)
        };
        state.value_damage.insert((player, av), damage);
    }
    if let Some(health) = profile
        .actor_values
        .iter()
        .find(|entry| u16::from(entry.actor_value) == world::combat::av::HEALTH)
    {
        let facts = world::scripting::Facts {
            order,
            state,
            speaker: None,
        };
        let endurance = facts
            .permanent_actor_value(player, world::combat::av::ENDURANCE)
            .unwrap_or(0.0);
        let health_modifier =
            world::magic::modifier(state, player, world::combat::av::HEALTH, false);
        let setting =
            |name: &str| f64::from(world::scripting::game_setting(order, name).unwrap_or(0.0));
        let base_health = f64::from(health.permanent)
            - health_modifier
            - setting("fAVDHealthEnduranceMult")
                * (endurance + setting("fAVDHealthEnduranceOffset"))
            - setting("fAVDHealthLevelMult") * (f64::from(profile.level.max(1)) - 1.0);
        state
            .actor_values
            .insert((player, world::combat::av::HEALTH), base_health);
        if let Some(maximum) = world::combat::max_health(order, state, player) {
            state
                .damage
                .insert(player, maximum - f64::from(health.current));
        }
    }

    let result = operation(state);

    state.actor_values.retain(|(who, _), _| *who != player);
    state.value_damage.retain(|(who, _), _| *who != player);
    for (av, value) in saved_actor_values {
        state.actor_values.insert((player, av), value);
    }
    for (av, value) in saved_value_damage {
        state.value_damage.insert((player, av), value);
    }
    state.damage.remove(&player);
    if let Some(damage) = saved_health_damage {
        state.damage.insert(player, damage);
    }
    state.perks = saved_perks;
    state.perk_ranks = saved_perk_ranks;
    state.player_level = saved_level;
    state.level_up_pending = saved_level_up_pending;
    result
}

fn with_coop_player_context<T>(
    state: &mut world::scripting::GameState,
    player: PlayerId,
    operation: impl FnOnce(&mut world::scripting::GameState) -> T,
) -> T {
    let previous = state.coop_player_context.replace(player.0);
    let result = operation(state);
    state.coop_player_context = previous;
    result
}

/// The host keeps each guest's campaign report under that peer. Configured
/// private quest/global facts, dialogue history, and reputation are installed
/// while the host resolves that guest's scripts, then restored to host state.
#[derive(Default)]
struct CampaignLedger {
    last_revision: Option<u32>,
    stages: HashMap<u32, u16>,
    stages_done: HashSet<(u32, u16)>,
    objectives: HashSet<(u32, i32)>,
    quest_status: HashMap<u32, (bool, bool, bool)>,
    quest_variables: HashMap<(u32, u16), f32>,
    globals: HashMap<u32, f32>,
    reputations: HashMap<u32, (f32, f32)>,
    dialogue_topics: HashSet<u32>,
    dialogue_said: HashSet<u32>,
    dialogue_talked_to: HashSet<u32>,
    player_female: Option<bool>,
    player_sex_initialized: bool,
    player_items: HashMap<u32, u32>,
    player_items_initialized: bool,
    player_actor_values: HashMap<(u8, u8), (f32, f32)>,
    player_misc_stats: HashMap<(u8, u8), u32>,
    player_misc_stats_initialized: HashSet<u8>,
}

#[derive(Default)]
struct PrivateCampaignSnapshot {
    stages: HashMap<FormId, u16>,
    stages_done: HashSet<(FormId, u16)>,
    running: HashSet<FormId>,
    completed: HashSet<FormId>,
    failed: HashSet<FormId>,
    objectives: Vec<((FormId, i32), bool)>,
    quest_variables: HashMap<(FormId, u16), f64>,
    variable_owners: HashSet<FormId>,
    globals: HashMap<FormId, f32>,
}

impl PrivateCampaignSnapshot {
    fn capture(
        order: &LoadOrder,
        scripts: &world::scripting::ScriptCache,
        state: &world::scripting::GameState,
        quests: &HashSet<FormId>,
        globals: &HashSet<FormId>,
    ) -> Self {
        let mut quest_variables = HashMap::new();
        let variable_owners: HashSet<_> = quests
            .iter()
            .copied()
            .filter(|quest| state.variables.contains_key(quest))
            .collect();
        for &quest in quests {
            let Some(count) = world::scripting::quest_script_variable_count(order, scripts, quest)
            else {
                continue;
            };
            for index in 0..count.min(usize::from(u16::MAX) + 1) {
                let index = index as u16;
                if let Some(value) =
                    world::scripting::quest_script_variable(order, scripts, state, quest, index)
                {
                    quest_variables.insert((quest, index), value);
                }
            }
        }
        Self {
            stages: state
                .stages
                .iter()
                .filter(|(quest, _)| quests.contains(quest))
                .map(|(quest, stage)| (*quest, *stage))
                .collect(),
            stages_done: state
                .stages_done
                .iter()
                .filter(|(quest, _)| quests.contains(quest))
                .copied()
                .collect(),
            running: state.running.intersection(quests).copied().collect(),
            completed: state.completed.intersection(quests).copied().collect(),
            failed: state.failed.intersection(quests).copied().collect(),
            objectives: state
                .objectives
                .iter()
                .filter(|((quest, _), _)| quests.contains(quest))
                .map(|(key, completed)| (*key, *completed))
                .collect(),
            quest_variables,
            variable_owners,
            globals: state
                .globals
                .iter()
                .filter(|(global, _)| globals.contains(global))
                .map(|(global, value)| (*global, *value))
                .collect(),
        }
    }

    fn from_ledger(
        order: &LoadOrder,
        ledger: &CampaignLedger,
        quests: &HashSet<FormId>,
        globals: &HashSet<FormId>,
    ) -> Self {
        let stages = ledger
            .stages
            .iter()
            .filter(|(quest, _)| quests.contains(&FormId(**quest)))
            .map(|(quest, stage)| (FormId(*quest), *stage))
            .collect();
        let stages_done = ledger
            .stages_done
            .iter()
            .filter(|(quest, _)| quests.contains(&FormId(*quest)))
            .map(|(quest, stage)| (FormId(*quest), *stage))
            .collect();
        let mut running = HashSet::new();
        let mut completed = HashSet::new();
        let mut failed = HashSet::new();
        for (&quest, &(is_running, is_completed, is_failed)) in &ledger.quest_status {
            let quest = FormId(quest);
            if !quests.contains(&quest) {
                continue;
            }
            if is_running {
                running.insert(quest);
            }
            if is_completed {
                completed.insert(quest);
            }
            if is_failed {
                failed.insert(quest);
            }
        }
        let objectives = ledger
            .objectives
            .iter()
            .filter(|(quest, _)| quests.contains(&FormId(*quest)))
            .map(|(quest, objective)| ((FormId(*quest), *objective), true))
            .collect();
        let quest_variables = ledger
            .quest_variables
            .iter()
            .filter(|((quest, _), _)| quests.contains(&FormId(*quest)))
            .map(|(&(quest, index), &value)| ((FormId(quest), index), f64::from(value)))
            .collect();
        let variable_owners = ledger
            .quest_variables
            .keys()
            .filter(|(quest, _)| quests.contains(&FormId(*quest)))
            .map(|(quest, _)| FormId(*quest))
            .collect();
        let mut global_values: HashMap<_, _> = globals
            .iter()
            .filter_map(|&global| {
                world::scripting::global_default(order, global).map(|value| (global, value))
            })
            .collect();
        global_values.extend(
            ledger
                .globals
                .iter()
                .filter(|(global, _)| globals.contains(&FormId(**global)))
                .map(|(global, value)| (FormId(*global), *value)),
        );
        Self {
            stages,
            stages_done,
            running,
            completed,
            failed,
            objectives,
            quest_variables,
            variable_owners,
            globals: global_values,
        }
    }

    fn replace_in_state(
        self,
        order: &LoadOrder,
        scripts: &world::scripting::ScriptCache,
        state: &mut world::scripting::GameState,
        quests: &HashSet<FormId>,
        globals: &HashSet<FormId>,
    ) {
        state.stages.retain(|quest, _| !quests.contains(quest));
        state.stages.extend(self.stages);
        state
            .stages_done
            .retain(|(quest, _)| !quests.contains(quest));
        state.stages_done.extend(self.stages_done);
        state.running.retain(|quest| !quests.contains(quest));
        state.running.extend(self.running);
        state.completed.retain(|quest| !quests.contains(quest));
        state.completed.extend(self.completed);
        state.failed.retain(|quest| !quests.contains(quest));
        state.failed.extend(self.failed);
        state
            .objectives
            .retain(|(quest, _), _| !quests.contains(quest));
        state.objectives.extend(self.objectives);
        state.globals.retain(|global, _| !globals.contains(global));
        state.globals.extend(self.globals);
        for &quest in quests {
            state.variables.remove(&quest);
            if !self.variable_owners.contains(&quest) {
                continue;
            }
            let Some(count) = world::scripting::quest_script_variable_count(order, scripts, quest)
            else {
                continue;
            };
            for index in 0..count.min(usize::from(u16::MAX) + 1) {
                let index = index as u16;
                let value = self
                    .quest_variables
                    .get(&(quest, index))
                    .copied()
                    .unwrap_or(0.0);
                world::scripting::set_quest_script_variable(
                    order, scripts, state, quest, index, value,
                );
            }
        }
    }

    fn store_in_ledger(
        self,
        ledger: &mut CampaignLedger,
        quests: &HashSet<FormId>,
        globals: &HashSet<FormId>,
    ) {
        ledger
            .stages
            .retain(|quest, _| !quests.contains(&FormId(*quest)));
        ledger.stages.extend(
            self.stages
                .into_iter()
                .map(|(quest, stage)| (quest.0, stage)),
        );
        ledger
            .stages_done
            .retain(|(quest, _)| !quests.contains(&FormId(*quest)));
        ledger.stages_done.extend(
            self.stages_done
                .into_iter()
                .map(|(quest, stage)| (quest.0, stage)),
        );
        ledger
            .quest_status
            .retain(|quest, _| !quests.contains(&FormId(*quest)));
        let quest_ids: HashSet<_> = self
            .running
            .iter()
            .chain(&self.completed)
            .chain(&self.failed)
            .copied()
            .collect();
        for quest in quest_ids {
            let status = (
                self.running.contains(&quest),
                self.completed.contains(&quest),
                self.failed.contains(&quest),
            );
            if status != (false, false, false) {
                ledger.quest_status.insert(quest.0, status);
            }
        }
        ledger
            .objectives
            .retain(|(quest, _)| !quests.contains(&FormId(*quest)));
        ledger.objectives.extend(
            self.objectives
                .into_iter()
                .filter(|(_, completed)| *completed)
                .map(|((quest, objective), _)| (quest.0, objective)),
        );
        ledger
            .quest_variables
            .retain(|(quest, _), _| !quests.contains(&FormId(*quest)));
        ledger
            .quest_variables
            .extend(
                self.quest_variables
                    .into_iter()
                    .filter_map(|((quest, index), value)| {
                        let value = value as f32;
                        (value.is_finite() && value != 0.0).then_some(((quest.0, index), value))
                    }),
            );
        ledger
            .globals
            .retain(|global, _| !globals.contains(&FormId(*global)));
        ledger.globals.extend(
            self.globals
                .into_iter()
                .filter(|(_, value)| value.is_finite())
                .map(|(global, value)| (global.0, value)),
        );
    }
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
    pub fn open(
        mode: Option<CoopMode>,
        scene_key: u64,
        order: &LoadOrder,
        private_quest_queries: &[String],
        private_global_queries: &[String],
    ) -> io::Result<Option<Self>> {
        let Some(mode) = mode else {
            return Ok(None);
        };
        if !private_quest_queries.is_empty() && !matches!(mode, CoopMode::Host(_)) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "private quest scope can only be set by the co-op host",
            ));
        }
        if !private_global_queries.is_empty() && !matches!(mode, CoopMode::Host(_)) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "private global scope can only be set by the co-op host",
            ));
        }
        let mut private_quests = HashSet::new();
        for query in private_quest_queries {
            let id = if let Some(hex) = query
                .strip_prefix("0x")
                .or_else(|| query.strip_prefix("0X"))
            {
                u32::from_str_radix(hex, 16).ok().map(FormId)
            } else {
                order.form_by_editor_id(query)
            }
            .ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::InvalidInput,
                    format!("private co-op quest '{query}' was not found"),
                )
            })?;
            let valid_quest = order
                .get(id)
                .is_some_and(|record| record.entry.header.kind == esm::FourCC::new(b"QUST"));
            if !valid_quest {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    format!("private co-op quest '{query}' is not a QUST record"),
                ));
            }
            private_quests.insert(id);
        }
        let mut private_globals = HashSet::new();
        for query in private_global_queries {
            let id = if let Some(hex) = query
                .strip_prefix("0x")
                .or_else(|| query.strip_prefix("0X"))
            {
                u32::from_str_radix(hex, 16).ok().map(FormId)
            } else {
                order.form_by_editor_id(query)
            }
            .ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::InvalidInput,
                    format!("private co-op global '{query}' was not found"),
                )
            })?;
            if world::scripting::global_default(order, id).is_none() {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    format!("private co-op global '{query}' is not a valid GLOB record"),
                ));
            }
            private_globals.insert(id);
        }
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
            private_quests,
            private_globals,
            tick: 0,
            accumulator: 0.0,
            next_send: now,
            next_hello: now,
            input_sequence: 0,
            dialogue_sequence: 0,
            last_dialogue_sequence: None,
            dialogue_choice_sequence: 0,
            dialogue_choices: VecDeque::new(),
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
            confirmed_killers: HashMap::new(),
            world_keyframe: ClientWorldKeyframe {
                next_transfer_id: world_keyframe_seed,
                pending_transfer_id: None,
                next_request: now,
                completed_transfer_id: None,
                assembly: None,
            },
            campaign_revision: 0,
            campaign_events: VecDeque::new(),
            shared_campaign_revision: 0,
            shared_campaign_events: VecDeque::new(),
            last_shared_campaign_revision: None,
            published_host_campaign_revision: 0,
            shared_campaign: CampaignLedger::default(),
            observed_stages: HashMap::new(),
            observed_stages_done: HashSet::new(),
            observed_objectives: HashSet::new(),
            observed_quest_status: HashMap::new(),
            observed_quest_variables: HashMap::new(),
            observed_globals: HashMap::new(),
            observed_reputations: HashMap::new(),
            observed_dialogue_topics: HashSet::new(),
            observed_dialogue_said: HashSet::new(),
            observed_dialogue_talked_to: HashSet::new(),
            observed_player_female: None,
            player_sex_observed: false,
            observed_player_items: HashMap::new(),
            player_items_observed: false,
            observed_player_misc_stats: HashMap::new(),
            player_misc_stats_observed: false,
            aim_sequence: 0,
            aim_target: None,
            last_aim_result_sequence: None,
            validated_aim: None,
            next_aim_request: now,
            vats_attack_sequence: 0,
            vats_attack: None,
            player_attack_sequence: 0,
            player_attacks: VecDeque::new(),
            inventory_initialized: false,
            inventory_revision: 0,
            inventory_ack_revision: None,
            inventory_entries: Vec::new(),
            profile_initialized: false,
            profile_revision: 0,
            profile_ack_revision: None,
            profile_level: 1,
            profile_actor_values: Vec::new(),
            profile_perks: Vec::new(),
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
            &game.0.order,
            &mut session.campaign_revision,
            &mut session.campaign_events,
            &mut session.observed_stages,
            &mut session.observed_stages_done,
            &mut session.observed_objectives,
            &mut session.observed_quest_status,
            &mut session.observed_quest_variables,
            &mut session.observed_globals,
            &mut session.observed_reputations,
            &mut session.observed_dialogue_topics,
            &mut session.observed_dialogue_said,
            &mut session.observed_dialogue_talked_to,
            &mut session.observed_player_female,
            &mut session.player_sex_observed,
            &mut session.observed_player_items,
            &mut session.player_items_observed,
            &state.0,
        );
        if let Some(player) = session.assigned_player {
            capture_player_misc_stats(
                player,
                &mut session.campaign_revision,
                &mut session.campaign_events,
                &mut session.observed_player_misc_stats,
                &mut session.player_misc_stats_observed,
                &state.0.misc_stats,
            );
        }
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
        capture_campaign_changes(
            &game.0.order,
            &mut session.campaign_revision,
            &mut session.campaign_events,
            &mut session.observed_stages,
            &mut session.observed_stages_done,
            &mut session.observed_objectives,
            &mut session.observed_quest_status,
            &mut session.observed_quest_variables,
            &mut session.observed_globals,
            &mut session.observed_reputations,
            &mut session.observed_dialogue_topics,
            &mut session.observed_dialogue_said,
            &mut session.observed_dialogue_talked_to,
            &mut session.observed_player_female,
            &mut session.player_sex_observed,
            &mut session.observed_player_items,
            &mut session.player_items_observed,
            &state.0,
        );
        capture_host_coop_player_updates(
            &mut session.campaign_revision,
            &mut session.campaign_events,
            &mut state.0,
        );
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
            publish_host_campaign_changes(
                &session.campaign_events,
                &mut session.published_host_campaign_revision,
                &session.private_quests,
                &session.private_globals,
                &mut session.shared_campaign,
                &mut session.shared_campaign_revision,
                &mut session.shared_campaign_events,
                peers,
            );
            session.campaign_events.clear();
            poll_host(
                socket,
                peers,
                session.scene_key,
                player.character.feet,
                &game.0.order,
                &scripts.0,
                vats_settings,
                &mut state.0,
                &session.private_quests,
                &session.private_globals,
                &mut session.shared_campaign,
                &mut session.shared_campaign_revision,
                &mut session.shared_campaign_events,
                &mut session.world_revision,
                &mut session.world_events,
                &mut session.observed_dead,
                &mut session.observed_destroyed,
                &collision.0,
                &collision,
                &walkers,
                &view.talkers,
                &view.cell_scripts,
                &view.rigs,
                exterior.as_deref(),
                real_time.elapsed_secs(),
                &mut incoming,
            );
            expire_stale_dialogue_lines(
                &game.0.order,
                &scripts.0,
                &mut state.0,
                &session.private_quests,
                &session.private_globals,
                peers,
                now,
            );
            listener_dialogue.0 = if local_dialogue.is_some() {
                None
            } else {
                nearby_dialogue(player.character.feet, PlayerId(0), None, peers, &walkers).and_then(
                    |(player, dialogue)| {
                        crate::dialogue::resolve_listener_dialogue(&game.0.order, &dialogue)
                            .map(|view| (player, view))
                    },
                )
            };
            regenerate_peer_action_points(
                peers,
                vats_settings,
                &game.0.order,
                &mut state.0,
                dt,
                now,
            );
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
                    let campaign_events: Vec<_> = session
                        .shared_campaign_events
                        .iter()
                        .filter(|event| {
                            sequence_is_newer(event.revision, peer.shared_campaign_revision)
                        })
                        .take(MAX_CAMPAIGN_EVENTS)
                        .copied()
                        .collect();
                    if !campaign_events.is_empty() {
                        let _ = send(
                            socket,
                            &Packet::CampaignSync {
                                events: campaign_events,
                            },
                            *address,
                        );
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
            let inventory_revision = session.inventory_revision;
            let profile_revision = session.profile_revision;
            poll_client(
                socket,
                *server,
                session.scene_key,
                &game.0.order,
                &scripts.0,
                &mut session.assigned_player,
                &mut session.last_received_tick,
                &mut session.poses,
                &mut session.last_world_revision,
                &mut session.confirmed_dead,
                &mut session.confirmed_destroyed,
                &mut session.confirmed_health,
                &mut session.confirmed_killers,
                &mut session.world_keyframe,
                &mut session.last_dialogue_sequence,
                &mut session.dialogue_choices,
                &mut session.last_shared_campaign_revision,
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
                &mut session.player_attacks,
                inventory_revision,
                &mut session.inventory_ack_revision,
                profile_revision,
                &mut session.profile_ack_revision,
                &mut state.0,
                now,
                &mut incoming,
            );
            if session.assigned_player.is_some() {
                if let Some(profile) = combat_profile_snapshot(&game.0.order, &state.0) {
                    if !session.profile_initialized
                        || profile.level != session.profile_level
                        || profile.actor_values != session.profile_actor_values
                        || profile.perks != session.profile_perks
                    {
                        session.profile_revision = session.profile_revision.wrapping_add(1);
                        if session.profile_revision == 0 {
                            session.profile_revision = 1;
                        }
                        session.profile_level = profile.level;
                        session.profile_actor_values = profile.actor_values;
                        session.profile_perks = profile.perks;
                        session.profile_initialized = true;
                        session.profile_ack_revision = None;
                        if let Some(request) = session
                            .vats_attack
                            .as_mut()
                            .filter(|request| request.decision.is_none())
                        {
                            request.profile_revision = session.profile_revision;
                        }
                    }
                }
                let attacks_pending = !session.player_attacks.is_empty()
                    || session
                        .vats_attack
                        .is_some_and(|request| request.decision.is_none());
                if !session.inventory_initialized || !attacks_pending {
                    let entries = combat_inventory_snapshot(
                        &game.0.order,
                        &state.0,
                        &session.player_attacks,
                        session.vats_attack,
                    );
                    if !session.inventory_initialized || entries != session.inventory_entries {
                        session.inventory_revision = session.inventory_revision.wrapping_add(1);
                        if session.inventory_revision == 0 {
                            session.inventory_revision = 1;
                        }
                        session.inventory_entries = entries;
                        session.inventory_initialized = true;
                        session.inventory_ack_revision = None;
                        if let Some(request) = session
                            .vats_attack
                            .as_mut()
                            .filter(|request| request.decision.is_none())
                        {
                            request.inventory_revision = session.inventory_revision;
                        }
                    }
                }
            }
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
                if session.assigned_player.is_some()
                    && session.inventory_initialized
                    && session.inventory_ack_revision != Some(session.inventory_revision)
                {
                    let _ = send(
                        socket,
                        &Packet::PlayerInventory {
                            revision: session.inventory_revision,
                            entries: session.inventory_entries.clone(),
                        },
                        *server,
                    );
                }
                if session.assigned_player.is_some()
                    && session.profile_initialized
                    && session.profile_ack_revision != Some(session.profile_revision)
                {
                    let _ = send(
                        socket,
                        &Packet::PlayerProfile {
                            revision: session.profile_revision,
                            level: session.profile_level,
                            actor_values: session.profile_actor_values.clone(),
                            perks: session.profile_perks.clone(),
                        },
                        *server,
                    );
                }
                if session.assigned_player.is_some()
                    && session.profile_ack_revision == Some(session.profile_revision)
                    && session.campaign_events.is_empty()
                {
                    if let Some(choice) = session.dialogue_choices.front() {
                        let _ = send(
                            socket,
                            &Packet::DialogueChoice {
                                sequence: choice.sequence,
                                offer: choice.offer.clone(),
                                source: choice.source,
                                topic: choice.topic,
                                info: choice.info,
                            },
                            *server,
                        );
                    }
                }
                if session.assigned_player.is_some()
                    && session.profile_ack_revision == Some(session.profile_revision)
                    && session.campaign_events.is_empty()
                    && now >= session.next_dialogue_send
                {
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
                if session.inventory_ack_revision == Some(session.inventory_revision)
                    && session.profile_ack_revision == Some(session.profile_revision)
                    && session.campaign_events.is_empty()
                {
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
                                inventory_revision: request.inventory_revision,
                                profile_revision: request.profile_revision,
                                ammo: request.ammo.map_or(0, |ammo| ammo.0),
                                kind: request.kind,
                                reload: request.reload,
                                slot: request.slot,
                                part_av: request.part_av,
                                action_points: request.action_points,
                            },
                            *server,
                        );
                    }
                }
                if session.assigned_player.is_some()
                    && session.inventory_ack_revision == Some(session.inventory_revision)
                    && session.profile_ack_revision == Some(session.profile_revision)
                    && session.campaign_events.is_empty()
                {
                    if let Some(request) = session.player_attacks.front().copied() {
                        let _ = send(
                            socket,
                            &Packet::PlayerAttack {
                                sequence: request.sequence,
                                inventory_revision: session.inventory_revision,
                                profile_revision: session.profile_revision,
                                weapon: request.weapon.map_or(0, |weapon| weapon.0),
                                ammo: request.ammo.map_or(0, |ammo| ammo.0),
                                attack_group: request.attack_group,
                                power: request.power,
                            },
                            *server,
                        );
                    }
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
    state: &mut world::scripting::GameState,
    dt: f32,
    now: Instant,
) {
    for peer in peers.values_mut() {
        let perk = if let Some(profile) = peer.profile.as_ref() {
            with_combat_profile(order, state, profile, |state| {
                world::perks::apply(order, state, world::perks::entry::ACTION_POINT_REGEN, 1.0)
            })
        } else {
            world::perks::apply(order, state, world::perks::entry::ACTION_POINT_REGEN, 1.0)
        };
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
    private_quests: &HashSet<FormId>,
    private_globals: &HashSet<FormId>,
    shared_campaign: &mut CampaignLedger,
    shared_campaign_revision: &mut u32,
    shared_campaign_events: &mut VecDeque<CampaignEvent>,
    world_revision: &mut u32,
    world_events: &mut VecDeque<WorldEvent>,
    observed_dead: &mut HashSet<u32>,
    observed_destroyed: &mut HashSet<u32>,
    collision: &physics::Collider,
    cell_collision: &CellCollision,
    walkers: &Query<&ai::Walker>,
    talkers: &crate::dialogue::Talkers,
    cell_scripts: &crate::scripts::CellScripts,
    rigs: &Query<(&ai::Walker, &crate::actors::ActorRig)>,
    exterior: Option<&crate::exterior::Exterior>,
    now_seconds: f32,
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
                    shared_campaign
                        .player_actor_values
                        .retain(|(player, _), _| *player != id.0);
                    shared_campaign
                        .player_misc_stats
                        .retain(|(player, _), _| *player != id.0);
                    shared_campaign.player_misc_stats_initialized.remove(&id.0);
                    state
                        .coop_misc_stats
                        .retain(|(player, _), _| *player != id.0);
                    state
                        .coop_actor_value_updates
                        .retain(|(player, _), _| *player != id.0);
                    state
                        .coop_misc_stat_updates
                        .retain(|(player, _), _| *player != id.0);
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
                    let accepted = apply_campaign_events(
                        &mut peer.campaign,
                        &events,
                        order,
                        scripts,
                        &mut campaign_quest_schemas,
                        peer.player,
                    );
                    for event in accepted {
                        match event.change {
                            CampaignChange::PlayerMiscStat {
                                player,
                                stat,
                                value,
                                authoritative: false,
                            } if player == peer.player.0 => {
                                if value == 0 {
                                    state.coop_misc_stats.remove(&(player, stat));
                                } else {
                                    state.coop_misc_stats.insert((player, stat), value);
                                }
                            }
                            _ => {}
                        }
                        if !campaign_change_is_shared(event.change, private_quests, private_globals)
                            || !campaign_change_is_valid(
                                order,
                                scripts,
                                &mut campaign_quest_schemas,
                                event.change,
                            )
                            || !campaign_change_would_update(shared_campaign, event.change)
                            || !apply_host_campaign_change(order, scripts, state, event.change)
                            || !apply_campaign_change_to_ledger(shared_campaign, event.change)
                        {
                            continue;
                        }
                        push_shared_campaign_event(
                            shared_campaign_revision,
                            shared_campaign_events,
                            event.change,
                        );
                    }
                }
            }
            Packet::CampaignSyncAck { revision } => {
                if let Some(peer) = peers.get_mut(&address) {
                    peer.last_seen = Instant::now();
                    if (revision == peer.shared_campaign_revision
                        || sequence_is_newer(revision, peer.shared_campaign_revision))
                        && (revision == *shared_campaign_revision
                            || sequence_is_newer(*shared_campaign_revision, revision))
                    {
                        peer.shared_campaign_revision = revision;
                    }
                }
            }
            Packet::PlayerProfile {
                revision,
                level,
                actor_values,
                perks,
            } => {
                if let Some(peer) = peers.get_mut(&address) {
                    peer.last_seen = Instant::now();
                    let is_current = peer.profile_revision == Some(revision);
                    let is_newer = peer
                        .profile_revision
                        .is_none_or(|previous| sequence_is_newer(revision, previous));
                    let mut profile = CombatProfile {
                        level,
                        actor_values,
                        perks,
                    };
                    for (&(player, actor_value), &(current, permanent)) in
                        &peer.campaign.player_actor_values
                    {
                        if player == peer.player.0 {
                            set_profile_actor_value(&mut profile, actor_value, current, permanent);
                        }
                    }
                    if (is_current || is_newer) && valid_combat_profile(order, &profile) {
                        if is_newer {
                            let previous_max = peer.max_action_points;
                            peer.profile = Some(profile);
                            peer.profile_revision = Some(revision);
                            if let Some(profile) = peer.profile.as_ref() {
                                peer.max_action_points =
                                    with_combat_profile(order, state, profile, |state| {
                                        world::vats::max_action_points(order, state)
                                    })
                                    .max(0.0);
                                if previous_max > 0.0 {
                                    peer.action_points = (peer.action_points / previous_max
                                        * peer.max_action_points)
                                        .clamp(0.0, peer.max_action_points);
                                } else {
                                    peer.action_points = peer.max_action_points;
                                }
                            }
                        }
                        let _ = send(socket, &Packet::PlayerProfileAck { revision }, address);
                    }
                }
            }
            Packet::PlayerInventory { revision, entries } => {
                if let Some(peer) = peers.get_mut(&address) {
                    peer.last_seen = Instant::now();
                    let is_current = peer.inventory_revision == Some(revision);
                    let is_newer = peer
                        .inventory_revision
                        .is_none_or(|previous| sequence_is_newer(revision, previous));
                    if (is_current || is_newer) && valid_combat_inventory(order, &entries) {
                        if is_newer {
                            peer.inventory = entries
                                .into_iter()
                                .map(|entry| (FormId(entry.form), entry.count))
                                .collect();
                            peer.inventory_revision = Some(revision);
                        }
                        let _ = send(socket, &Packet::PlayerInventoryAck { revision }, address);
                    }
                }
            }
            Packet::CampaignSync { .. } => {}
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
                        let explicitly_ended = dialogue.is_none();
                        let accepted = dialogue.filter(|dialogue| {
                            valid_dialogue_update(
                                order, state, collision, walkers, talkers, rigs, peer, dialogue,
                            )
                        });
                        let offer_is_fresh = peer
                            .dialogue_updated_at
                            .is_some_and(|updated| updated.elapsed() <= DIALOGUE_STATE_TIMEOUT);
                        if offer_is_fresh && (explicitly_ended || accepted.is_some()) {
                            if let Some(previous) = peer.dialogue.clone() {
                                apply_host_dialogue_transition(
                                    order,
                                    scripts,
                                    state,
                                    private_quests,
                                    private_globals,
                                    peer,
                                    &previous,
                                    accepted.as_ref(),
                                );
                            }
                        }
                        if explicitly_ended || accepted.is_some() {
                            peer.dialogue_updated_at = Some(Instant::now());
                        }
                        peer.dialogue = accepted;
                    }
                }
            }
            Packet::DialogueChoice {
                sequence,
                offer,
                source,
                topic,
                info,
            } => {
                if let Some(peer) = peers.get_mut(&address) {
                    peer.last_seen = Instant::now();
                    let cached = peer
                        .last_dialogue_choice_result
                        .filter(|(cached_sequence, _)| *cached_sequence == sequence);
                    let accepted = if let Some((_, accepted)) = cached {
                        Some(accepted)
                    } else if peer
                        .last_dialogue_choice_sequence
                        .is_some_and(|previous| !sequence_is_newer(sequence, previous))
                    {
                        None
                    } else {
                        peer.last_dialogue_choice_sequence = Some(sequence);
                        let speaker = FormId(offer.speaker);
                        let info_id = FormId(info);
                        let selected_info = world::dialogue::Info::load(order, info_id);
                        let valid = offer.line.is_none()
                            && offer
                                .topics
                                .iter()
                                .any(|choice| choice.topic == topic && choice.info == info)
                            && selected_info
                                .as_ref()
                                .is_some_and(|record| record.topic == Some(FormId(topic)))
                            && valid_dialogue_update(
                                order, state, collision, walkers, talkers, rigs, peer, &offer,
                            );
                        let valid = valid
                            && dialogue_choice_is_available(
                                order,
                                scripts,
                                state,
                                private_quests,
                                private_globals,
                                peer,
                                &offer,
                                source,
                                topic,
                                info,
                            );
                        let private = selected_info
                            .as_ref()
                            .and_then(|record| record.quest)
                            .is_some_and(|quest| private_quests.contains(&quest));
                        let same_line = peer.host_dialogue_line == Some((speaker.0, info_id.0));
                        if valid {
                            peer.campaign
                                .dialogue_topics
                                .extend(offer.topics.iter().map(|choice| choice.topic));
                        }
                        if !valid {
                            Some(false)
                        } else if same_line {
                            Some(true)
                        } else {
                            if let Some((old_speaker, old_info)) = peer.host_dialogue_line.take() {
                                apply_guest_dialogue_script(
                                    order,
                                    scripts,
                                    state,
                                    private_quests,
                                    private_globals,
                                    peer,
                                    FormId(old_speaker),
                                    FormId(old_info),
                                    false,
                                );
                            }
                            if private {
                                peer.dialogue_updated_at = Some(Instant::now());
                                Some(true)
                            } else {
                                let applied = apply_guest_dialogue_script(
                                    order,
                                    scripts,
                                    state,
                                    private_quests,
                                    private_globals,
                                    peer,
                                    speaker,
                                    info_id,
                                    true,
                                );
                                if applied {
                                    peer.host_dialogue_line = Some((speaker.0, info_id.0));
                                    peer.dialogue_updated_at = Some(Instant::now());
                                }
                                Some(applied)
                            }
                        }
                    };
                    if let Some(accepted) = accepted {
                        if accepted {
                            peer.dialogue_updated_at = Some(Instant::now());
                        }
                        peer.last_dialogue_choice_result = Some((sequence, accepted));
                        let _ = send(
                            socket,
                            &Packet::DialogueChoiceResult { sequence, accepted },
                            address,
                        );
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
                inventory_revision,
                profile_revision,
                ammo,
                kind,
                reload,
                slot,
                part_av,
                action_points,
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
                    let profile = (peer.profile_revision == Some(profile_revision)
                        && peer
                            .campaign
                            .player_misc_stats_initialized
                            .contains(&peer.player.0))
                    .then(|| peer.profile.clone())
                    .flatten();
                    let loadout = (peer.inventory_revision == Some(inventory_revision)
                        && profile.is_some())
                    .then(|| peer_attack_loadout(order, peer, weapon, ammo))
                    .flatten();
                    let attack_ammo = loadout.as_ref().and_then(|(_, ammo, _)| *ammo);
                    let ammo_use = loadout.as_ref().map_or(0, |(_, _, ammo_use)| *ammo_use);
                    let target_valid = loadout.is_some()
                        && profile.as_ref().is_some_and(|profile| {
                            with_combat_profile(order, state, profile, |state| {
                                validate_vats_target(
                                    order,
                                    vats_settings,
                                    state,
                                    collision,
                                    walkers,
                                    peer,
                                    FormId(target),
                                )
                            })
                        });
                    let target_down = state.more.down.contains_key(&FormId(target))
                        || walkers
                            .iter()
                            .find(|walker| walker.reference == FormId(target))
                            .is_some_and(ai::Walker::is_fallen);
                    let expected_cost = profile.as_ref().and_then(|profile| {
                        with_combat_profile(order, state, profile, |state| {
                            expected_vats_attack_cost(
                                order,
                                state,
                                vats_settings,
                                target_down,
                                FormId(weapon),
                                kind,
                                reload,
                            )
                        })
                    });
                    let part_valid = valid_vats_part(order, state, FormId(target), slot, part_av);
                    let hit_chance = if target_valid && part_valid {
                        profile.as_ref().and_then(|profile| {
                            with_combat_profile(order, state, profile, |state| {
                                host_vats_hit_chance(
                                    order,
                                    vats_settings,
                                    state,
                                    collision,
                                    walkers,
                                    peer,
                                    FormId(target),
                                    FormId(weapon),
                                    slot,
                                    part_av,
                                )
                            })
                        })
                    } else {
                        None
                    };
                    let mut accepted = false;
                    let mut hit = false;
                    if let Some(cost) = expected_cost.filter(|cost| cost.is_finite() && *cost > 0.0)
                    {
                        let tolerance = (cost * 0.01).max(0.05);
                        let cost_matches =
                            action_points.is_finite() && (action_points - cost).abs() <= tolerance;
                        if let Some(hit_chance) = hit_chance {
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
                    }
                    if accepted && hit {
                        if let Some(profile) = profile.as_ref() {
                            with_combat_profile(order, state, profile, |state| {
                                with_coop_player_context(state, peer.player, |state| {
                                    apply_host_vats_shot(
                                        order,
                                        scripts,
                                        state,
                                        FormId(target),
                                        FormId(weapon),
                                        kind,
                                        slot,
                                        part_av,
                                        attack_ammo,
                                        peer.character.feet,
                                        peer.input.sneaking,
                                        walkers,
                                    );
                                });
                            });
                            if let Some(profile) = peer.profile.as_mut() {
                                update_profile_from_coop_actor_values(profile, peer.player, state);
                            }
                        }
                    }
                    if accepted {
                        consume_peer_ammo(peer, attack_ammo, ammo_use);
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
            Packet::PlayerAttack {
                sequence,
                inventory_revision,
                profile_revision,
                weapon,
                ammo,
                attack_group,
                power,
            } => {
                if let Some(peer) = peers.get_mut(&address) {
                    peer.last_seen = Instant::now();
                    let cached = peer
                        .last_player_attack_result
                        .filter(|(cached_sequence, _)| *cached_sequence == sequence);
                    let accepted = if let Some((_, accepted)) = cached {
                        accepted
                    } else if peer
                        .last_player_attack_sequence
                        .is_some_and(|previous| !sequence_is_newer(sequence, previous))
                    {
                        false
                    } else {
                        peer.last_player_attack_sequence = Some(sequence);
                        let interval = if weapon == 0 {
                            0.5
                        } else {
                            world::combat::Weapon::load(order, FormId(weapon))
                                .map_or(0.5, |weapon| weapon.shot_interval())
                        };
                        let cadence_ok = peer
                            .last_player_attack_at
                            .is_none_or(|at| at.elapsed().as_secs_f32() >= interval * 0.65);
                        let profile = (peer.profile_revision == Some(profile_revision)
                            && peer
                                .campaign
                                .player_misc_stats_initialized
                                .contains(&peer.player.0))
                        .then(|| peer.profile.clone())
                        .flatten();
                        let loadout = (peer.inventory_revision == Some(inventory_revision)
                            && profile.is_some())
                        .then(|| peer_attack_loadout(order, peer, weapon, ammo))
                        .flatten();
                        let attack_ammo = loadout.as_ref().and_then(|(_, ammo, _)| *ammo);
                        let ammo_use = loadout.as_ref().map_or(0, |(_, _, ammo_use)| *ammo_use);
                        let accepted = loadout.is_some()
                            && cadence_ok
                            && profile.as_ref().is_some_and(|profile| {
                                with_combat_profile(order, state, profile, |state| {
                                    with_coop_player_context(state, peer.player, |state| {
                                        apply_host_player_attack(
                                            order,
                                            scripts,
                                            state,
                                            cell_collision,
                                            talkers,
                                            cell_scripts,
                                            rigs,
                                            peer,
                                            FormId(weapon),
                                            attack_ammo,
                                            attack_group,
                                            power,
                                            now_seconds,
                                        )
                                    })
                                })
                            });
                        if let Some(profile) = peer.profile.as_mut() {
                            update_profile_from_coop_actor_values(profile, peer.player, state);
                        }
                        if accepted {
                            peer.last_player_attack_at = Some(Instant::now());
                            consume_peer_ammo(peer, attack_ammo, ammo_use);
                            if world::combat::Weapon::load(order, FormId(weapon))
                                .is_some_and(|weapon| world::explosions::is_thrown(&weapon))
                            {
                                let exhausted = peer
                                    .inventory
                                    .get_mut(&FormId(weapon))
                                    .is_some_and(|count| {
                                        *count = count.saturating_sub(1);
                                        *count == 0
                                    });
                                if exhausted {
                                    peer.inventory.remove(&FormId(weapon));
                                }
                            }
                        }
                        peer.last_player_attack_result = Some((sequence, accepted));
                        accepted
                    };
                    let _ = send(
                        socket,
                        &Packet::PlayerAttackResult { sequence, accepted },
                        address,
                    );
                }
            }
            _ => {}
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn apply_host_player_attack(
    order: &esm::LoadOrder,
    scripts: &world::scripting::ScriptCache,
    state: &mut world::scripting::GameState,
    cell_collision: &CellCollision,
    talkers: &crate::dialogue::Talkers,
    cell_scripts: &crate::scripts::CellScripts,
    rigs: &Query<(&ai::Walker, &crate::actors::ActorRig)>,
    peer: &mut Peer,
    weapon_id: FormId,
    ammo: Option<FormId>,
    attack_group: u8,
    power: bool,
    now: f32,
) -> bool {
    use crate::combat::{self, Met};
    use world::combat::Weapon;
    use world::scripting::Runner;

    let weapon = if weapon_id == FormId(0) {
        None
    } else {
        let Some(weapon) = Weapon::load(order, weapon_id) else {
            return false;
        };
        Some(weapon)
    };
    let melee = weapon.as_ref().is_none_or(Weapon::is_melee);
    if power && !melee {
        return false;
    }
    if let Some(weapon) = weapon
        .as_ref()
        .filter(|weapon| world::explosions::is_thrown(weapon))
    {
        if weapon
            .projectile
            .and_then(|id| world::explosions::ProjectileRecord::load(order, id))
            .is_none()
        {
            return false;
        }
        let (sin_heading, cos_heading) = peer.input.heading.sin_cos();
        let (sin_pitch, cos_pitch) = peer.input.pitch.sin_cos();
        let direction = [sin_heading * cos_pitch, cos_heading * cos_pitch, sin_pitch];
        let origin = [
            peer.character.feet[0],
            peer.character.feet[1],
            peer.character.feet[2] + cellview::EYE_HEIGHT,
        ];
        crate::explosives::throw(crate::explosives::Launch {
            thrower: world::dialogue::PLAYER_REF,
            coop_player: Some(peer.player.0),
            combat_profile: combat_profile_snapshot(order, state),
            weapon: weapon.clone(),
            origin,
            aim: crate::explosives::Aim::Along(direction),
            consume_item: false,
        });
        return true;
    }
    if melee && world::melee::group_file_stem(attack_group).is_none() {
        return false;
    }
    let (count, cone, reach) = match &weapon {
        Some(weapon) if !melee => {
            let (count, cone) = weapon.shot(order, ammo);
            if count == 0 || count > 64 {
                return false;
            }
            (
                count,
                cone,
                weapon.range(order).unwrap_or(crate::combat::SHOT_RANGE),
            )
        }
        _ => (
            1,
            0.0,
            Weapon::melee_reach(weapon.as_ref()) + physics::CharacterShape::PLAYER.radius,
        ),
    };
    let pellet = weapon.clone().map(|mut weapon| {
        weapon.damage /= count as f32;
        if !melee {
            weapon.projectile = world::combat::fired_projectile_with_ammo(order, &weapon, ammo);
        }
        weapon
    });
    let heading = peer.input.heading;
    let pitch = peer.input.pitch;
    let eye = [
        peer.character.feet[0],
        peer.character.feet[1],
        peer.character.feet[2] + cellview::EYE_HEIGHT,
    ];
    let cones = world::melee::Cones::read(order);
    let swing = melee.then(|| world::melee::Swing {
        at: peer.character.feet,
        heading,
        radius: physics::CharacterShape::PLAYER.radius,
        bound_z: world::npc_aim::bound_z(order, world::dialogue::PLAYER_BASE).unwrap_or([0.0; 2]),
        swimming: false,
        slope_difference: world::scripting::game_setting(order, "fAICombatSlopeDifference")
            .unwrap_or(48.0),
        reach: world::melee::swing_reach(order, weapon.as_ref(), 1.0, false),
        cone: cones.for_attack(attack_group),
        dead_mult: cones.dead_mult,
        player: true,
        vats_playback: false,
    });
    let old_sneaking = state.player_sneaking;
    state.player_sneaking = peer.input.sneaking;
    let mut hit_count = 0usize;
    let mut projectile_count = 0usize;
    for _ in 0..count {
        let unit = |value: u64| (value % 1_000_000) as f32 / 1_000_000.0;
        let radius = cone * unit(state.roll());
        let theta = std::f32::consts::TAU * unit(state.roll());
        let (h, p) = (heading + radius * theta.cos(), pitch + radius * theta.sin());
        let direction = [h.sin() * p.cos(), h.cos() * p.cos(), p.sin()];
        if let Some(bolt) = pellet
            .as_ref()
            .filter(|weapon| !melee && crate::bolts::flies(order, weapon))
        {
            crate::bolts::fire(
                order,
                state,
                world::dialogue::PLAYER_REF,
                bolt,
                (eye, direction),
            );
            projectile_count += 1;
            continue;
        }
        let met = if let Some(swing) = swing.as_ref() {
            combat::melee_met(
                order,
                state,
                &mut peer.attack_cache,
                (talkers, cell_scripts, cell_collision, rigs),
                (eye, direction),
                swing,
                reach,
                now,
            )
        } else {
            combat::first_met(
                order,
                state,
                &mut peer.attack_cache,
                talkers,
                cell_scripts,
                cell_collision,
                rigs,
                (eye, direction),
                reach,
                false,
                now,
            )
        };
        let Met::Thing {
            reference, part, ..
        } = met
        else {
            continue;
        };
        let special = if melee {
            world::melee::special_of(weapon.as_ref(), attack_group)
        } else {
            world::melee::Special::None
        };
        if Runner::new(order, scripts, state)
            .blow_at_with_ammo(
                world::dialogue::PLAYER_REF,
                reference,
                pellet.as_ref(),
                part,
                world::melee::Blow {
                    power: power && !peer.input.sneaking,
                    special,
                },
                ammo,
            )
            .is_some()
        {
            hit_count += 1;
        }
    }
    state.player_sneaking = old_sneaking;
    println!(
        "Co-op host resolved player {} attack: {} actor hit(s), {} projectile(s)",
        peer.player.0, hit_count, projectile_count
    );
    // An attack can be accepted even if its authoritative ray found no actor.
    true
}

fn roll_vats_hit(state: &mut world::scripting::GameState, hit_chance: f32) -> bool {
    hit_chance.is_finite()
        && (0.0..=1.0).contains(&hit_chance)
        && ((state.roll() % 1_000_000) as f32 / 1_000_000.0) < hit_chance
}

fn host_vats_hit_chance(
    order: &esm::LoadOrder,
    settings: &world::vats::Settings,
    state: &mut world::scripting::GameState,
    collision: &physics::Collider,
    walkers: &Query<&ai::Walker>,
    peer: &mut Peer,
    target: FormId,
    weapon_id: FormId,
    slot: i8,
    part_av: i32,
) -> Option<f32> {
    let target_walker = walkers.iter().find(|walker| walker.reference == target)?;
    let body = world::body_parts::BodyPartData::of(order, target);
    let part = if slot >= 0 {
        body.as_ref()
            .and_then(|body| body.part(u8::try_from(slot).ok()?))
    } else {
        None
    };
    if slot >= 0
        && slot != world::body_parts::part::WEAPON as i8
        && part.is_none_or(|part| i32::from(part.actor_value) != part_av)
    {
        return None;
    }
    let weapon = if weapon_id == FormId(0) {
        None
    } else {
        Some(world::combat::Weapon::load(order, weapon_id)?)
    };

    let offset = [
        target_walker.position[0] - peer.character.feet[0],
        target_walker.position[1] - peer.character.feet[1],
        target_walker.position[2] - peer.character.feet[2],
    ];
    let center_distance = offset.iter().map(|value| value * value).sum::<f32>().sqrt();
    let gap = center_distance
        - world::vats::bound_of(order, target).max(0.0) * 0.5
        - physics::CharacterShape::PLAYER.radius;
    let eye = [
        peer.character.feet[0],
        peer.character.feet[1],
        peer.character.feet[2] + cellview::EYE_HEIGHT,
    ];
    let target_radius = world::vats::bound_of(order, target).max(0.0) * 0.5;
    let aim_point = [
        target_walker.position[0],
        target_walker.position[1],
        target_walker.position[2] + target_radius,
    ];
    let visible = if crate::fighting::clear_ray_between(collision, eye, aim_point) {
        1.0
    } else {
        0.0
    };
    let chance = world::vats::part_chance(
        order,
        state,
        settings,
        &world::vats::ChanceQuery {
            target,
            part,
            part_av,
            weapon: weapon.as_ref(),
            distance: gap.max(0.0),
            visible,
            stance: world::vats::Stance::SCANNING,
        },
    );
    (chance.is_finite() && (0.0..=1.0).contains(&chance)).then_some(chance)
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
    ammo: Option<FormId>,
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
        world::scripting::Runner::new(order, scripts, state).blow_at_with_ammo(
            world::dialogue::PLAYER_REF,
            target,
            weapon.as_ref(),
            part,
            blow,
            ammo,
        )
    } else {
        world::scripting::Runner::new(order, scripts, state).blow_at_with_ammo(
            world::dialogue::PLAYER_REF,
            target,
            weapon.as_ref(),
            part,
            world::melee::Blow {
                power: false,
                special: world::melee::Special::None,
            },
            ammo,
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
            shared_campaign_revision: 0,
            last_world_update_revision: None,
            last_aim_sequence: None,
            action_points: max_action_points,
            max_action_points,
            last_vats_activity: None,
            last_attack_sequence: None,
            last_attack_result: None,
            last_player_attack_sequence: None,
            last_player_attack_result: None,
            last_player_attack_at: None,
            attack_cache: crate::combat::PlayerAttack::default(),
            inventory_revision: None,
            inventory: HashMap::new(),
            profile_revision: None,
            profile: None,
            vats_attack: None,
            last_world_keyframe_request: None,
            world_keyframe: None,
            last_dialogue_sequence: None,
            last_dialogue_choice_sequence: None,
            last_dialogue_choice_result: None,
            dialogue_updated_at: None,
            dialogue: None,
            host_dialogue_line: None,
        },
    );
    Some(id)
}

fn combat_inventory_snapshot(
    order: &esm::LoadOrder,
    state: &world::scripting::GameState,
    player_attacks: &VecDeque<PlayerAttackRequest>,
    vats_attack: Option<VatsAttackRequest>,
) -> Vec<InventoryEntry> {
    let player = world::dialogue::PLAYER_REF;
    let mut weapons = Vec::new();
    if let Some(weapon) = world::dps::equipped_weapon(order, state, player) {
        weapons.push(weapon);
    }
    weapons.extend(player_attacks.iter().filter_map(|attack| attack.weapon));
    if let Some(request) = vats_attack {
        if request.decision.is_none() {
            weapons.extend(request.weapon);
        }
    }
    weapons.sort_unstable();
    weapons.dedup();

    let pending_ammo: HashMap<FormId, u32> =
        player_attacks
            .iter()
            .fold(HashMap::new(), |mut pending, attack| {
                if let Some(ammo) = attack.ammo {
                    let count = pending.entry(ammo).or_default();
                    *count = count.saturating_add(attack.ammo_use);
                }
                pending
            });
    let mut counts = HashMap::<FormId, u32>::new();
    for weapon_id in weapons {
        let Some(weapon) = world::combat::Weapon::load(order, weapon_id) else {
            continue;
        };
        let held = state.item_count(order, player, weapon_id).max(0) as u32;
        if held > 0 || world::dps::equipped_weapon(order, state, player) == Some(weapon_id) {
            counts.insert(weapon_id, held.max(1));
        }
        for ammo in weapon.ammo {
            let held = state.item_count(order, player, ammo).max(0) as u32;
            let pending = pending_ammo.get(&ammo).copied().unwrap_or_default();
            let count = held.saturating_add(pending);
            if count > 0 {
                counts.insert(ammo, count);
            }
        }
    }
    for attack in player_attacks {
        if let Some(ammo) = attack.ammo {
            let held = state.item_count(order, player, ammo).max(0) as u32;
            let pending = pending_ammo.get(&ammo).copied().unwrap_or_default();
            let count = held.saturating_add(pending);
            if count > 0 {
                counts.insert(ammo, count);
            }
        }
    }
    let mut entries: Vec<_> = counts
        .into_iter()
        .filter(|(_, count)| *count > 0)
        .map(|(form, count)| InventoryEntry {
            form: form.0,
            count: count.min(i32::MAX as u32),
        })
        .collect();
    entries.sort_by_key(|entry| entry.form);
    entries.truncate(MAX_PLAYER_INVENTORY_ENTRIES);
    entries
}

fn valid_combat_inventory(order: &esm::LoadOrder, entries: &[InventoryEntry]) -> bool {
    entries.len() <= MAX_PLAYER_INVENTORY_ENTRIES
        && entries.iter().all(|entry| {
            entry.count <= i32::MAX as u32
                && order.get(FormId(entry.form)).is_some_and(|record| {
                    matches!(record.entry.header.kind.as_bytes(), b"WEAP" | b"AMMO")
                })
        })
}

fn valid_combat_profile(order: &esm::LoadOrder, profile: &CombatProfile) -> bool {
    if profile.actor_values.len() > coop_sync_experiment::MAX_PLAYER_PROFILE_ACTOR_VALUES
        || profile.perks.len() > MAX_PLAYER_PROFILE_PERKS
        || profile.level == 0
    {
        return false;
    }
    let actor_values_valid = profile.actor_values.iter().all(|entry| {
        usize::from(entry.actor_value) < coop_sync_experiment::MAX_PLAYER_PROFILE_ACTOR_VALUES
            && entry.current.is_finite()
            && entry.permanent.is_finite()
    });
    let perks_valid = profile.perks.iter().all(|perk| {
        perk.rank > 0
            && order
                .get(FormId(perk.form))
                .is_some_and(|record| record.entry.header.kind == esm::FourCC::new(b"PERK"))
    });
    let actor_ids: HashSet<_> = profile
        .actor_values
        .iter()
        .map(|entry| entry.actor_value)
        .collect();
    let perk_ids: HashSet<_> = profile.perks.iter().map(|perk| perk.form).collect();
    actor_values_valid
        && perks_valid
        && actor_ids.len() == profile.actor_values.len()
        && perk_ids.len() == profile.perks.len()
}

fn peer_attack_loadout(
    order: &esm::LoadOrder,
    peer: &mut Peer,
    weapon_id: u32,
    ammo_id: u32,
) -> Option<(Option<world::combat::Weapon>, Option<FormId>, u32)> {
    if weapon_id == 0 {
        return (ammo_id == 0).then_some((None, None, 0));
    }
    let weapon_id = FormId(weapon_id);
    let weapon = world::combat::Weapon::load(order, weapon_id)?;
    if peer.inventory.get(&weapon_id).copied().unwrap_or_default() == 0 {
        return None;
    }
    if weapon.ammo.is_empty() {
        return (ammo_id == 0).then_some((Some(weapon), None, 0));
    }
    let ammo = FormId(ammo_id);
    if ammo_id == 0
        || !weapon.ammo.contains(&ammo)
        || !order
            .get(ammo)
            .is_some_and(|record| record.entry.header.kind.as_bytes() == b"AMMO")
    {
        return None;
    }
    let ammo_use = u32::from(weapon.ammo_use.max(1));
    (peer.inventory.get(&ammo).copied().unwrap_or_default() >= ammo_use).then_some((
        Some(weapon),
        Some(ammo),
        ammo_use,
    ))
}

fn consume_peer_ammo(peer: &mut Peer, ammo: Option<FormId>, ammo_use: u32) {
    if ammo_use == 0 {
        return;
    }
    let Some(ammo) = ammo else {
        return;
    };
    let exhausted = peer.inventory.get_mut(&ammo).is_some_and(|count| {
        *count = count.saturating_sub(ammo_use);
        *count == 0
    });
    if exhausted {
        peer.inventory.remove(&ammo);
    }
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
    let forward = [sin_heading * cos_pitch, cos_heading * cos_pitch, sin_pitch];
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

fn valid_dialogue_update(
    order: &esm::LoadOrder,
    state: &world::scripting::GameState,
    collision: &physics::Collider,
    walkers: &Query<&ai::Walker>,
    talkers: &crate::dialogue::Talkers,
    rigs: &Query<(&ai::Walker, &crate::actors::ActorRig)>,
    peer: &Peer,
    dialogue: &DialogueSnapshot,
) -> bool {
    let speaker_ref = FormId(dialogue.speaker);
    let Some(_walker) = walkers
        .iter()
        .find(|walker| walker.reference == speaker_ref)
    else {
        return false;
    };
    if state.dead.contains(&speaker_ref) || !world::enabled_now(order, speaker_ref, &state.disabled)
    {
        return false;
    }
    let Some(base) = world::scripting::base_of(order, speaker_ref) else {
        return false;
    };
    let Some(_speaker) = world::dialogue::Speaker::load(order, speaker_ref, base) else {
        return false;
    };
    if !valid_dialogue_records(order, dialogue) {
        return false;
    }

    let capsules = crate::crosshair::people_shapes(talkers, state, rigs);
    let eye = [
        peer.character.feet[0],
        peer.character.feet[1],
        peer.character.feet[2] + cellview::EYE_HEIGHT,
    ];
    let (sin_heading, cos_heading) = peer.input.heading.sin_cos();
    let (sin_pitch, cos_pitch) = peer.input.pitch.sin_cos();
    let world = physics::view_caster::World {
        collider: collision,
        capsules: &capsules,
        origin: eye,
        direction: [sin_heading * cos_pitch, cos_heading * cos_pitch, sin_pitch],
        fuzzy_replaces: |reference| world::activation::fuzzy_replaces(order, FormId(reference)),
        excluded: |_| false,
    };
    world
        .pick(
            world::activation::PICK_LENGTH,
            world::activation::PICK_RADIUS,
            world::dialogue::PLAYER_REF.0,
            &physics::view_caster::Settings::default(),
        )
        .is_some_and(|pick| pick.reference == speaker_ref.0)
}

fn valid_dialogue_records(order: &esm::LoadOrder, dialogue: &DialogueSnapshot) -> bool {
    if dialogue.line.is_some() && !dialogue.topics.is_empty() {
        return false;
    }
    if let Some(line) = dialogue.line {
        let Some(info) = world::dialogue::Info::load(order, FormId(line.info)) else {
            return false;
        };
        let Some(topic) = info.topic else {
            return false;
        };
        if usize::from(line.response) >= info.responses.len()
            || !world::dialogue::topic_lines(order, topic)
                .iter()
                .any(|candidate| candidate.form_id.0 == line.info)
        {
            return false;
        }
    }
    dialogue.topics.iter().all(|choice| {
        let Some(topic) = world::dialogue::Topic::load(order, FormId(choice.topic)) else {
            return false;
        };
        let Some(info) = world::dialogue::Info::load(order, FormId(choice.info)) else {
            return false;
        };
        info.responses.first().is_some()
            && world::dialogue::topic_lines(order, topic.form_id)
                .iter()
                .any(|candidate| candidate.form_id == info.form_id)
    })
}

/// Rebuild the available response list from the host's loaded records and the
/// guest's synchronized campaign/player facts. The client still controls its
/// menu presentation, but cannot request an INFO whose conditions are false
/// for that guest at the time the host accepts the choice.
fn dialogue_choice_is_available(
    order: &esm::LoadOrder,
    scripts: &world::scripting::ScriptCache,
    state: &mut world::scripting::GameState,
    private_quests: &HashSet<FormId>,
    private_globals: &HashSet<FormId>,
    peer: &Peer,
    offer: &DialogueSnapshot,
    source: Option<u32>,
    topic: u32,
    info: u32,
) -> bool {
    let Some(profile) = peer.profile.clone() else {
        return false;
    };
    let speaker_ref = FormId(offer.speaker);
    let Some(base) = world::scripting::base_of(order, speaker_ref) else {
        return false;
    };
    let Some(speaker) = world::dialogue::Speaker::load(order, speaker_ref, base) else {
        return false;
    };

    if let Some(source) = source {
        let source = FormId(source);
        if !peer.campaign.dialogue_said.contains(&source.0)
            || !world::dialogue::Info::load(order, source)
                .is_some_and(|line| !line.choices.is_empty())
        {
            return false;
        }
    }

    // Dialogue conditions can inspect gender and the player's inventory. Do
    // not approve a choice until the guest has sent its initial snapshot for
    // both, otherwise host-only player data could produce a false decision.
    if !peer.campaign.player_sex_initialized || !peer.campaign.player_items_initialized {
        return false;
    }

    let host_private_campaign =
        PrivateCampaignSnapshot::capture(order, scripts, state, private_quests, private_globals);
    PrivateCampaignSnapshot::from_ledger(order, &peer.campaign, private_quests, private_globals)
        .replace_in_state(order, scripts, state, private_quests, private_globals);

    let host_player_female = state.player_female;
    state.player_female = peer.campaign.player_female;
    let host_player_items: HashMap<FormId, i32> = state
        .items
        .iter()
        .filter_map(|(&(owner, item), &count)| {
            (owner == world::dialogue::PLAYER_REF).then_some((item, count))
        })
        .collect();
    state
        .items
        .retain(|(owner, _), _| *owner != world::dialogue::PLAYER_REF);
    state.items.extend(
        peer.campaign
            .player_items
            .iter()
            .map(|(&form, &count)| ((world::dialogue::PLAYER_REF, FormId(form)), count as i32)),
    );

    let host_topics = std::mem::replace(
        &mut state.topics,
        peer.campaign
            .dialogue_topics
            .iter()
            .copied()
            .map(FormId)
            .collect(),
    );
    let host_said = std::mem::replace(
        &mut state.said,
        peer.campaign
            .dialogue_said
            .iter()
            .copied()
            .map(FormId)
            .collect(),
    );
    let host_talked_to = std::mem::replace(
        &mut state.talked_to,
        peer.campaign
            .dialogue_talked_to
            .iter()
            .copied()
            .map(FormId)
            .collect(),
    );
    let host_reputations = std::mem::replace(
        &mut state.reputations,
        peer.campaign
            .reputations
            .iter()
            .map(|(&form, &values)| (FormId(form), values))
            .collect(),
    );

    let available = with_coop_player_context(state, peer.player, |state| {
        with_combat_profile(order, state, &profile, |state| {
            let top_level = world::dialogue::top_level_topics(order);
            let choices = if let Some(source) = source {
                let Some(line) = world::dialogue::Info::load(order, FormId(source)) else {
                    return false;
                };
                world::dialogue::next_choices(order, &line, &top_level, &speaker, state)
            } else {
                world::dialogue::menu_topics(order, &top_level, &speaker, state)
            };
            choices.iter().any(|choice| choice.topic.form_id.0 == topic)
                && dialogue_info_is_available(order, state, &speaker, FormId(topic), FormId(info))
        })
    });

    state.topics = host_topics;
    state.said = host_said;
    state.talked_to = host_talked_to;
    state.reputations = host_reputations;
    state.player_female = host_player_female;
    state
        .items
        .retain(|(owner, _), _| *owner != world::dialogue::PLAYER_REF);
    state.items.extend(
        host_player_items
            .into_iter()
            .map(|(item, count)| ((world::dialogue::PLAYER_REF, item), count)),
    );
    host_private_campaign.replace_in_state(order, scripts, state, private_quests, private_globals);
    available
}

/// Check a selected response against topic ordering and all host-evaluable
/// conditions. A random INFO group may pick a different member on each peer,
/// so any eligible member in that same group is accepted.
fn dialogue_info_is_available(
    order: &esm::LoadOrder,
    state: &world::scripting::GameState,
    speaker: &world::dialogue::Speaker,
    topic: FormId,
    selected: FormId,
) -> bool {
    let facts = world::scripting::Facts {
        order,
        state,
        speaker: Some(speaker),
    };
    let mut quest_conditions = HashMap::new();
    let mut random_group = Vec::new();
    for info in world::dialogue::topic_lines(order, topic)
        .into_iter()
        .filter(|info| {
            if let Some(quest) = info.quest {
                let available = *quest_conditions.entry(quest).or_insert_with(|| {
                    state.running.contains(&quest)
                        && facts.conditions_pass(
                            &world::quest::quest_conditions(order, quest),
                            speaker.reference,
                            world::dialogue::PLAYER_REF,
                        )
                });
                if !available {
                    return false;
                }
            }
            world::dialogue::line_available(
                order,
                info,
                speaker,
                world::dialogue::PLAYER_REF,
                state,
            )
        })
    {
        if info.flags & world::dialogue::RANDOM == 0 {
            if random_group.is_empty() {
                return info.form_id == selected;
            }
            break;
        }
        random_group.push(info.form_id);
        if info.flags & world::dialogue::RANDOM_END != 0 {
            break;
        }
    }
    random_group.contains(&selected)
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
    for peer in peers.values().filter(|peer| {
        peer.player != listener
            && peer
                .dialogue_updated_at
                .is_some_and(|updated| updated.elapsed() <= DIALOGUE_STATE_TIMEOUT)
    }) {
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
    scripts: &world::scripting::ScriptCache,
    assigned: &mut Option<PlayerId>,
    last_tick: &mut Option<u32>,
    poses: &mut HashMap<PlayerId, PlayerPose>,
    last_world_revision: &mut Option<u32>,
    confirmed_dead: &mut HashSet<u32>,
    confirmed_destroyed: &mut HashSet<u32>,
    confirmed_health: &mut HashMap<u32, i32>,
    confirmed_killers: &mut HashMap<u32, u8>,
    world_keyframe: &mut ClientWorldKeyframe,
    last_dialogue_sequence: &mut Option<u32>,
    dialogue_choices: &mut VecDeque<PendingDialogueChoice>,
    last_shared_campaign_revision: &mut Option<u32>,
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
    player_attacks: &mut VecDeque<PlayerAttackRequest>,
    inventory_revision: u32,
    inventory_ack_revision: &mut Option<u32>,
    profile_revision: u32,
    profile_ack_revision: &mut Option<u32>,
    state: &mut world::scripting::GameState,
    now: Instant,
    incoming: &mut [u8; MAX_PACKET_BYTES],
) {
    let mut campaign_quest_schemas = HashMap::new();
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
                        confirmed_killers,
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
                        confirmed_killers,
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
                            confirmed_killers.clear();
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
                                confirmed_killers,
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
                listener_dialogue.0 = dialogue.and_then(|dialogue| {
                    crate::dialogue::resolve_listener_dialogue(order, &dialogue)
                        .map(|view| (player, view))
                });
            }
            Ok(Packet::DialogueChoiceResult { sequence, accepted })
                if dialogue_choices
                    .front()
                    .is_some_and(|choice| choice.sequence == sequence) =>
            {
                dialogue_choices.pop_front();
                if !accepted {
                    eprintln!("Co-op host rejected the dialogue choice.");
                }
            }
            Ok(Packet::CampaignSync { events }) if assigned.is_some() => {
                for event in &events {
                    if last_shared_campaign_revision
                        .is_some_and(|previous| !sequence_is_newer(event.revision, previous))
                    {
                        continue;
                    }
                    let expected = last_shared_campaign_revision
                        .unwrap_or_default()
                        .wrapping_add(1);
                    if event.revision != expected {
                        eprintln!(
                            "Co-op shared quest event gap: expected revision {expected}, received {}",
                            event.revision
                        );
                        break;
                    }
                    if matches!(event.change, CampaignChange::Reputation { .. })
                        || !campaign_change_is_valid(
                            order,
                            scripts,
                            &mut campaign_quest_schemas,
                            event.change,
                        )
                    {
                        eprintln!(
                            "Rejected invalid co-op shared quest event at revision {}",
                            event.revision
                        );
                    } else {
                        if let Some(player) = *assigned {
                            apply_client_campaign_change(
                                order,
                                scripts,
                                state,
                                player,
                                *profile_ack_revision == Some(profile_revision),
                                event.change,
                            );
                        }
                    }
                    *last_shared_campaign_revision = Some(event.revision);
                }
                if let Some(revision) = *last_shared_campaign_revision {
                    let _ = send(socket, &Packet::CampaignSyncAck { revision }, server);
                }
            }
            Ok(Packet::CampaignSyncAck { .. }) => {}
            Ok(Packet::PlayerInventoryAck { revision })
                if assigned.is_some() && revision == inventory_revision =>
            {
                *inventory_ack_revision = Some(revision);
            }
            Ok(Packet::PlayerProfileAck { revision })
                if assigned.is_some() && revision == profile_revision =>
            {
                *profile_ack_revision = Some(revision);
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
            Ok(Packet::PlayerAttackResult { sequence, accepted })
                if player_attacks
                    .front()
                    .is_some_and(|request| request.sequence == sequence) =>
            {
                if accepted {
                    if let Some(weapon) = player_attacks
                        .front()
                        .and_then(|request| request.weapon)
                        .and_then(|weapon| world::combat::Weapon::load(order, weapon))
                        .filter(world::explosions::is_thrown)
                    {
                        crate::explosives::use_up(
                            order,
                            state,
                            world::dialogue::PLAYER_REF,
                            weapon.form_id,
                        );
                    }
                }
                player_attacks.pop_front();
                if !accepted {
                    eprintln!("Co-op host rejected player attack {sequence}.");
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
            .map(|reference| WorldChange::ActorDied {
                reference,
                player: state.coop_killers.get(&FormId(reference)).copied(),
            }),
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
    order: &esm::LoadOrder,
    revision: &mut u32,
    journal: &mut VecDeque<CampaignEvent>,
    known_stages: &mut HashMap<u32, u16>,
    known_stages_done: &mut HashSet<(u32, u16)>,
    known_objectives: &mut HashSet<(u32, i32)>,
    known_quest_status: &mut HashMap<u32, (bool, bool, bool)>,
    known_quest_variables: &mut HashMap<(u32, u16), f32>,
    known_globals: &mut HashMap<u32, f32>,
    known_reputations: &mut HashMap<u32, (f32, f32)>,
    known_dialogue_topics: &mut HashSet<u32>,
    known_dialogue_said: &mut HashSet<u32>,
    known_dialogue_talked_to: &mut HashSet<u32>,
    known_player_female: &mut Option<bool>,
    player_sex_observed: &mut bool,
    known_player_items: &mut HashMap<u32, u32>,
    player_items_observed: &mut bool,
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

    let mut quest_variables = HashMap::new();
    for (&quest, locals) in &state.variables {
        if !order
            .get(quest)
            .is_some_and(|record| record.entry.header.kind == esm::FourCC::new(b"QUST"))
        {
            continue;
        }
        for (index, (_, _, value)) in locals.iter().enumerate() {
            let Ok(index) = u16::try_from(index) else {
                break;
            };
            let value = value as f32;
            if value.is_finite() && value != 0.0 {
                quest_variables.insert((quest.0, index), value);
            }
        }
    }
    let mut variable_ids: HashSet<_> = known_quest_variables.keys().copied().collect();
    variable_ids.extend(quest_variables.keys().copied());
    let mut variable_ids: Vec<_> = variable_ids.into_iter().collect();
    variable_ids.sort_unstable();
    for (quest, index) in variable_ids {
        let value = quest_variables.get(&(quest, index)).copied().unwrap_or(0.0);
        if known_quest_variables
            .get(&(quest, index))
            .copied()
            .unwrap_or(0.0)
            != value
        {
            push_campaign_event(
                revision,
                journal,
                CampaignChange::QuestVariable {
                    quest,
                    index,
                    value,
                },
            );
        }
    }
    *known_quest_variables = quest_variables;

    let globals: HashMap<u32, f32> = state
        .globals
        .iter()
        .filter_map(|(global, value)| {
            let base = world::scripting::global_default(order, *global)?;
            (value.is_finite() && *value != base).then_some((global.0, *value))
        })
        .collect();
    let mut global_ids: HashSet<_> = known_globals.keys().copied().collect();
    global_ids.extend(globals.keys().copied());
    let mut global_ids: Vec<_> = global_ids.into_iter().collect();
    global_ids.sort_unstable();
    for global in global_ids {
        let base = world::scripting::global_default(order, FormId(global)).unwrap_or(0.0);
        let old = known_globals.get(&global).copied().unwrap_or(base);
        let new = globals.get(&global).copied().unwrap_or(base);
        if old != new {
            push_campaign_event(
                revision,
                journal,
                CampaignChange::GlobalValue { global, value: new },
            );
        }
    }
    *known_globals = globals;

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

    capture_dialogue_facts(
        revision,
        journal,
        DialogueFactKind::Topic,
        state.topics.iter().map(|form| form.0).collect(),
        known_dialogue_topics,
    );
    capture_dialogue_facts(
        revision,
        journal,
        DialogueFactKind::Said,
        state.said.iter().map(|form| form.0).collect(),
        known_dialogue_said,
    );
    capture_dialogue_facts(
        revision,
        journal,
        DialogueFactKind::TalkedTo,
        state.talked_to.iter().map(|form| form.0).collect(),
        known_dialogue_talked_to,
    );

    if !*player_sex_observed || *known_player_female != state.player_female {
        push_campaign_event(
            revision,
            journal,
            CampaignChange::PlayerSex {
                female: state.player_female,
            },
        );
        *known_player_female = state.player_female;
        *player_sex_observed = true;
    }

    let player = world::dialogue::PLAYER_REF;
    let player_items: HashMap<u32, u32> = state
        .items
        .iter()
        .filter_map(|(&(owner, item), &count)| {
            (owner == player
                && count > 0
                && order
                    .get(item)
                    .is_some_and(|record| world::scripting::is_item(record.entry.header.kind)))
            .then_some((item.0, count as u32))
        })
        .collect();
    let mut item_forms: HashSet<u32> = known_player_items.keys().copied().collect();
    item_forms.extend(player_items.keys().copied());
    let mut item_forms: Vec<_> = item_forms.into_iter().collect();
    item_forms.sort_unstable();
    for form in item_forms {
        let count = player_items.get(&form).copied().unwrap_or_default();
        if known_player_items.get(&form).copied().unwrap_or_default() != count {
            push_campaign_event(
                revision,
                journal,
                CampaignChange::PlayerItemCount { form, count },
            );
        }
    }
    *known_player_items = player_items;
    if !*player_items_observed {
        push_campaign_event(revision, journal, CampaignChange::PlayerItemsInitialized);
        *player_items_observed = true;
    }
}

fn capture_player_misc_stats(
    player: PlayerId,
    revision: &mut u32,
    journal: &mut VecDeque<CampaignEvent>,
    known: &mut HashMap<u8, u32>,
    initialized: &mut bool,
    current: &BTreeMap<u8, u32>,
) {
    let current: HashMap<u8, u32> = current
        .iter()
        .filter_map(|(&stat, &value)| (value != 0).then_some((stat, value)))
        .collect();
    let mut stats: HashSet<_> = known.keys().copied().collect();
    stats.extend(current.keys().copied());
    let mut stats: Vec<_> = stats.into_iter().collect();
    stats.sort_unstable();
    for stat in stats {
        let value = current.get(&stat).copied().unwrap_or_default();
        if known.get(&stat).copied().unwrap_or_default() != value {
            push_campaign_event(
                revision,
                journal,
                CampaignChange::PlayerMiscStat {
                    player: player.0,
                    stat,
                    value,
                    authoritative: false,
                },
            );
        }
    }
    *known = current;
    if !*initialized {
        push_campaign_event(
            revision,
            journal,
            CampaignChange::PlayerMiscStatsInitialized { player: player.0 },
        );
        *initialized = true;
    }
}

fn capture_host_coop_player_updates(
    revision: &mut u32,
    journal: &mut VecDeque<CampaignEvent>,
    state: &mut world::scripting::GameState,
) {
    let mut actor_values: Vec<_> = std::mem::take(&mut state.coop_actor_value_updates)
        .into_iter()
        .collect();
    actor_values.sort_unstable_by_key(|((player, actor_value), _)| (*player, *actor_value));
    for ((player, actor_value), (current, permanent)) in actor_values {
        push_campaign_event(
            revision,
            journal,
            CampaignChange::PlayerActorValue {
                player,
                actor_value: actor_value as u8,
                current,
                permanent,
            },
        );
    }

    let mut stats: Vec<_> = std::mem::take(&mut state.coop_misc_stat_updates)
        .into_iter()
        .collect();
    stats.sort_unstable_by_key(|((player, stat), _)| (*player, *stat));
    for ((player, stat), value) in stats {
        push_campaign_event(
            revision,
            journal,
            CampaignChange::PlayerMiscStat {
                player,
                stat,
                value,
                authoritative: true,
            },
        );
    }
}

fn capture_dialogue_facts(
    revision: &mut u32,
    journal: &mut VecDeque<CampaignEvent>,
    kind: DialogueFactKind,
    current: HashSet<u32>,
    known: &mut HashSet<u32>,
) {
    for form in ordered_difference(&current, known) {
        push_campaign_event(
            revision,
            journal,
            CampaignChange::DialogueFact {
                kind,
                form,
                learned: true,
            },
        );
    }
    for form in ordered_difference(known, &current) {
        push_campaign_event(
            revision,
            journal,
            CampaignChange::DialogueFact {
                kind,
                form,
                learned: false,
            },
        );
    }
    *known = current;
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
    variable_count: usize,
}

fn campaign_quest_schema<'a>(
    order: &esm::LoadOrder,
    scripts: &world::scripting::ScriptCache,
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
                variable_count: quest
                    .script
                    .and_then(|script| scripts.script(order, script))
                    .map_or(0, |script| script.variables.len()),
            })
        })
        .as_ref()
}

fn campaign_change_is_valid(
    order: &esm::LoadOrder,
    scripts: &world::scripting::ScriptCache,
    schemas: &mut HashMap<u32, Option<CampaignQuestSchema>>,
    change: CampaignChange,
) -> bool {
    match change {
        CampaignChange::QuestStage { quest, stage } => {
            campaign_quest_schema(order, scripts, quest, schemas)
                .is_some_and(|schema| stage == 0 || schema.stages.contains(&stage))
        }
        CampaignChange::QuestStageDone { quest, stage, .. } => {
            campaign_quest_schema(order, scripts, quest, schemas)
                .is_some_and(|schema| schema.stages.contains(&stage))
        }
        CampaignChange::QuestObjective {
            quest, objective, ..
        } => campaign_quest_schema(order, scripts, quest, schemas)
            .is_some_and(|schema| schema.objectives.contains(&objective)),
        CampaignChange::QuestStatus { quest, .. } => {
            campaign_quest_schema(order, scripts, quest, schemas).is_some()
        }
        CampaignChange::QuestVariable {
            quest,
            index,
            value,
        } => {
            value.is_finite()
                && campaign_quest_schema(order, scripts, quest, schemas)
                    .is_some_and(|schema| usize::from(index) < schema.variable_count)
        }
        CampaignChange::Reputation { reputation, .. } => order
            .get(FormId(reputation))
            .is_some_and(|record| record.entry.header.kind == esm::FourCC::new(b"REPU")),
        CampaignChange::GlobalValue { global, value } => {
            value.is_finite() && world::scripting::global_default(order, FormId(global)).is_some()
        }
        CampaignChange::DialogueFact { kind, form, .. } => {
            let Some(record) = order.get(FormId(form)) else {
                return false;
            };
            let record_kind = record.entry.header.kind;
            match kind {
                DialogueFactKind::Topic => record_kind == esm::FourCC::new(b"DIAL"),
                DialogueFactKind::Said => record_kind == esm::FourCC::new(b"INFO"),
                DialogueFactKind::TalkedTo => [
                    esm::FourCC::new(b"ACHR"),
                    esm::FourCC::new(b"ACRE"),
                    esm::FourCC::new(b"REFR"),
                    esm::FourCC::new(b"NPC_"),
                    esm::FourCC::new(b"CREA"),
                ]
                .contains(&record_kind),
            }
        }
        CampaignChange::PlayerSex { .. } | CampaignChange::PlayerItemsInitialized => true,
        CampaignChange::PlayerItemCount { form, count } => {
            count <= i32::MAX as u32
                && order
                    .get(FormId(form))
                    .is_some_and(|record| world::scripting::is_item(record.entry.header.kind))
        }
        CampaignChange::PlayerActorValue {
            player,
            actor_value,
            current,
            permanent,
        } => {
            usize::from(player) < MAX_PLAYERS
                && usize::from(actor_value) < coop_sync_experiment::MAX_PLAYER_PROFILE_ACTOR_VALUES
                && current.is_finite()
                && permanent.is_finite()
        }
        CampaignChange::PlayerMiscStat { player, .. }
        | CampaignChange::PlayerMiscStatsInitialized { player } => {
            usize::from(player) < MAX_PLAYERS
        }
    }
}

fn campaign_change_is_shared(
    change: CampaignChange,
    private_quests: &HashSet<FormId>,
    private_globals: &HashSet<FormId>,
) -> bool {
    let quest = match change {
        CampaignChange::QuestStage { quest, .. }
        | CampaignChange::QuestStageDone { quest, .. }
        | CampaignChange::QuestObjective { quest, .. }
        | CampaignChange::QuestStatus { quest, .. }
        | CampaignChange::QuestVariable { quest, .. } => quest,
        CampaignChange::GlobalValue { global, .. } => {
            return !private_globals.contains(&FormId(global))
        }
        CampaignChange::DialogueFact { .. }
        | CampaignChange::PlayerSex { .. }
        | CampaignChange::PlayerItemCount { .. }
        | CampaignChange::PlayerItemsInitialized
        | CampaignChange::PlayerMiscStatsInitialized { .. } => return false,
        CampaignChange::PlayerActorValue { .. } => return true,
        CampaignChange::PlayerMiscStat { authoritative, .. } => return authoritative,
        CampaignChange::Reputation { .. } => return false,
    };
    !private_quests.contains(&FormId(quest))
}

fn apply_campaign_change_to_state(
    order: &esm::LoadOrder,
    scripts: &world::scripting::ScriptCache,
    state: &mut world::scripting::GameState,
    change: CampaignChange,
) {
    match change {
        CampaignChange::QuestStage { quest, stage } => {
            let quest = FormId(quest);
            if stage == 0 {
                state.stages.remove(&quest);
            } else {
                let current = state.stages.entry(quest).or_insert(0);
                *current = (*current).max(stage);
            }
        }
        CampaignChange::QuestStageDone { quest, stage, done } => {
            if done {
                state.stages_done.insert((FormId(quest), stage));
            } else {
                state.stages_done.remove(&(FormId(quest), stage));
            }
        }
        CampaignChange::QuestObjective {
            quest,
            objective,
            completed,
        } => {
            if completed {
                state.objectives.insert((FormId(quest), objective), true);
            } else {
                state.objectives.remove(&(FormId(quest), objective));
            }
        }
        CampaignChange::QuestStatus {
            quest,
            running,
            completed,
            failed,
        } => {
            let quest = FormId(quest);
            state.running.remove(&quest);
            state.completed.remove(&quest);
            state.failed.remove(&quest);
            if running {
                state.running.insert(quest);
            }
            if completed {
                state.completed.insert(quest);
            }
            if failed {
                state.failed.insert(quest);
            }
        }
        CampaignChange::QuestVariable {
            quest,
            index,
            value,
        } => {
            world::scripting::set_quest_script_variable(
                order,
                scripts,
                state,
                FormId(quest),
                index,
                f64::from(value),
            );
        }
        CampaignChange::GlobalValue { global, value } => {
            state.globals.insert(FormId(global), value);
        }
        CampaignChange::DialogueFact { .. } => {}
        CampaignChange::PlayerSex { .. }
        | CampaignChange::PlayerItemCount { .. }
        | CampaignChange::PlayerItemsInitialized
        | CampaignChange::PlayerActorValue { .. }
        | CampaignChange::PlayerMiscStat { .. }
        | CampaignChange::PlayerMiscStatsInitialized { .. } => {}
        CampaignChange::Reputation { .. } => {}
    }
}

fn apply_host_campaign_change(
    order: &esm::LoadOrder,
    scripts: &world::scripting::ScriptCache,
    state: &mut world::scripting::GameState,
    change: CampaignChange,
) -> bool {
    match change {
        CampaignChange::QuestStage { quest, stage } => {
            if !world::scripting::Runner::new(order, scripts, state).set_stage(FormId(quest), stage)
            {
                return false;
            }
            state
                .stages
                .get(&FormId(quest))
                .is_some_and(|current| *current >= stage)
                && state.stages_done.contains(&(FormId(quest), stage))
        }
        CampaignChange::Reputation { .. }
        | CampaignChange::DialogueFact { .. }
        | CampaignChange::PlayerSex { .. }
        | CampaignChange::PlayerItemCount { .. }
        | CampaignChange::PlayerItemsInitialized
        | CampaignChange::PlayerActorValue { .. }
        | CampaignChange::PlayerMiscStat { .. }
        | CampaignChange::PlayerMiscStatsInitialized { .. } => false,
        CampaignChange::QuestVariable {
            quest,
            index,
            value,
        } => world::scripting::quest_script_variable(order, scripts, state, FormId(quest), index)
            .is_some_and(|current| current as f32 == value),
        CampaignChange::GlobalValue { global, value } => state
            .globals
            .get(&FormId(global))
            .is_some_and(|current| *current == value),
        // A peer's report cannot set shared objective/status flags. The host
        // accepts them only when its own simulation (usually the preceding
        // host-run quest stage and result script) already produced that state.
        _ => campaign_change_matches_state(order, scripts, state, change),
    }
}

fn campaign_change_matches_state(
    order: &esm::LoadOrder,
    scripts: &world::scripting::ScriptCache,
    state: &world::scripting::GameState,
    change: CampaignChange,
) -> bool {
    match change {
        CampaignChange::QuestStage { quest, stage } => {
            state
                .stages
                .get(&FormId(quest))
                .is_some_and(|current| *current >= stage)
                && state.stages_done.contains(&(FormId(quest), stage))
        }
        CampaignChange::QuestStageDone { quest, stage, done } => {
            state.stages_done.contains(&(FormId(quest), stage)) == done
        }
        CampaignChange::QuestObjective {
            quest,
            objective,
            completed,
        } => {
            state
                .objectives
                .get(&(FormId(quest), objective))
                .copied()
                .unwrap_or(false)
                == completed
        }
        CampaignChange::QuestStatus {
            quest,
            running,
            completed,
            failed,
        } => {
            let quest = FormId(quest);
            state.running.contains(&quest) == running
                && state.completed.contains(&quest) == completed
                && state.failed.contains(&quest) == failed
        }
        CampaignChange::Reputation { .. }
        | CampaignChange::DialogueFact { .. }
        | CampaignChange::PlayerSex { .. }
        | CampaignChange::PlayerItemCount { .. }
        | CampaignChange::PlayerItemsInitialized
        | CampaignChange::PlayerActorValue { .. }
        | CampaignChange::PlayerMiscStat { .. }
        | CampaignChange::PlayerMiscStatsInitialized { .. } => false,
        CampaignChange::QuestVariable {
            quest,
            index,
            value,
        } => world::scripting::quest_script_variable(order, scripts, state, FormId(quest), index)
            .is_some_and(|current| current as f32 == value),
        CampaignChange::GlobalValue { global, value } => state
            .globals
            .get(&FormId(global))
            .is_some_and(|current| *current == value),
    }
}

fn apply_client_campaign_change(
    order: &esm::LoadOrder,
    scripts: &world::scripting::ScriptCache,
    state: &mut world::scripting::GameState,
    player: PlayerId,
    profile_ready: bool,
    change: CampaignChange,
) {
    match change {
        CampaignChange::PlayerActorValue {
            player: recipient,
            actor_value,
            current,
            permanent,
        } if recipient == player.0 => {
            if profile_ready {
                apply_player_actor_value(order, state, actor_value, current, permanent);
            }
            return;
        }
        CampaignChange::PlayerMiscStat {
            player: recipient,
            stat,
            value,
            authoritative: true,
        } if recipient == player.0 => {
            if profile_ready {
                if value == 0 {
                    state.misc_stats.remove(&stat);
                } else {
                    state.misc_stats.insert(stat, value);
                }
            }
            return;
        }
        _ => {}
    }
    if let CampaignChange::QuestStage { quest, stage } = change {
        if world::scripting::Runner::new(order, scripts, state).set_stage(FormId(quest), stage) {
            return;
        }
    }
    apply_campaign_change_to_state(order, scripts, state, change);
}

fn apply_player_actor_value(
    order: &esm::LoadOrder,
    state: &mut world::scripting::GameState,
    actor_value: u8,
    current: f32,
    permanent: f32,
) {
    let who = world::dialogue::PLAYER_REF;
    let av = u16::from(actor_value);
    let permanent_modifier = world::magic::modifier(state, who, av, true);
    let current_modifier = world::magic::modifier(state, who, av, false);
    let base = f64::from(permanent) - permanent_modifier;
    state.actor_values.insert((who, av), base);
    let damage = if world::magic::COUNTERS.contains(&av) {
        f64::from(current) - current_modifier
    } else {
        f64::from(permanent) + current_modifier - permanent_modifier - f64::from(current)
    };
    state.value_damage.insert((who, av), damage);
    if av == world::experience::XP {
        state.level_up_pending = state.player_level < world::experience::max_level(order)
            && f64::from(current) >= world::experience::xp_for_level(order, state.player_level + 1);
    }
}

/// Mirror dialogue transitions after the host has acknowledged a selection.
/// A follow-up line can also begin when its INFO record lists it from the
/// active host-tracked line.
fn apply_host_dialogue_transition(
    order: &esm::LoadOrder,
    scripts: &world::scripting::ScriptCache,
    state: &mut world::scripting::GameState,
    private_quests: &HashSet<FormId>,
    private_globals: &HashSet<FormId>,
    peer: &mut Peer,
    previous: &DialogueSnapshot,
    next: Option<&DialogueSnapshot>,
) {
    let next_for_speaker = next.filter(|dialogue| dialogue.speaker == previous.speaker);
    let previous_line = previous.line;

    if let Some(line) = previous_line {
        let line_ended = next_for_speaker.is_none_or(|dialogue| {
            dialogue
                .line
                .is_none_or(|next_line| next_line.info != line.info)
                || !dialogue.topics.is_empty()
        });
        if line_ended && peer.host_dialogue_line == Some((previous.speaker, line.info)) {
            apply_guest_dialogue_script(
                order,
                scripts,
                state,
                private_quests,
                private_globals,
                peer,
                FormId(previous.speaker),
                FormId(line.info),
                false,
            );
            peer.host_dialogue_line = None;
        }
    }

    let Some(line) = next_for_speaker.and_then(|dialogue| dialogue.line) else {
        return;
    };
    if line.response != 0 {
        return;
    }
    let speaker = FormId(previous.speaker);
    let info = FormId(line.info);
    let follow_up = previous_line.is_some_and(|line| {
        world::dialogue::Info::load(order, FormId(line.info))
            .is_some_and(|previous_info| previous_info.follow_ups.contains(&info))
    });
    if follow_up && peer.host_dialogue_line != Some((speaker.0, info.0)) {
        if apply_guest_dialogue_script(
            order,
            scripts,
            state,
            private_quests,
            private_globals,
            peer,
            speaker,
            info,
            true,
        ) {
            peer.host_dialogue_line = Some((speaker.0, info.0));
        }
    }
}

fn expire_stale_dialogue_lines(
    order: &esm::LoadOrder,
    scripts: &world::scripting::ScriptCache,
    state: &mut world::scripting::GameState,
    private_quests: &HashSet<FormId>,
    private_globals: &HashSet<FormId>,
    peers: &mut HashMap<SocketAddr, Peer>,
    now: Instant,
) {
    for peer in peers.values_mut() {
        let stale = peer
            .dialogue_updated_at
            .is_none_or(|updated| now.saturating_duration_since(updated) > DIALOGUE_STATE_TIMEOUT);
        if stale {
            if let Some((speaker, info)) = peer.host_dialogue_line.take() {
                apply_guest_dialogue_script(
                    order,
                    scripts,
                    state,
                    private_quests,
                    private_globals,
                    peer,
                    FormId(speaker),
                    FormId(info),
                    false,
                );
            }
        }
    }
}

/// Run the guest's dialogue script against host world state while keeping
/// the legacy `PLAYER_REF` fields that belong to the host player private.
/// Quest progress for explicitly private faction quests stays on the guest.
fn apply_guest_dialogue_script(
    order: &esm::LoadOrder,
    scripts: &world::scripting::ScriptCache,
    state: &mut world::scripting::GameState,
    private_quests: &HashSet<FormId>,
    private_globals: &HashSet<FormId>,
    peer: &mut Peer,
    speaker: FormId,
    info_id: FormId,
    begin: bool,
) -> bool {
    let Some(info) = world::dialogue::Info::load(order, info_id) else {
        return false;
    };
    if info
        .quest
        .is_some_and(|quest| private_quests.contains(&quest))
    {
        return false;
    }
    let Some(profile) = peer.profile.clone() else {
        return false;
    };

    let player = world::dialogue::PLAYER_REF;
    let player_female = state.player_female;
    let host_private_campaign =
        PrivateCampaignSnapshot::capture(order, scripts, state, private_quests, private_globals);
    PrivateCampaignSnapshot::from_ledger(order, &peer.campaign, private_quests, private_globals)
        .replace_in_state(order, scripts, state, private_quests, private_globals);
    let player_items: HashMap<FormId, i32> = state
        .items
        .iter()
        .filter_map(|(&(owner, item), &count)| (owner == player).then_some((item, count)))
        .collect();
    if peer.campaign.player_items_initialized {
        state.items.retain(|(owner, _), _| *owner != player);
        state.items.extend(
            peer.campaign
                .player_items
                .iter()
                .map(|(&form, &count)| ((player, FormId(form)), count as i32)),
        );
    }
    if peer.campaign.player_sex_initialized {
        state.player_female = peer.campaign.player_female;
    }
    let player_equipment = state.equipped.get(&player).cloned();
    let player_topics = state.topics.clone();
    let player_said = state.said.clone();
    let player_talked_to = state.talked_to.clone();
    let player_reputations = state.reputations.clone();
    state.topics = peer
        .campaign
        .dialogue_topics
        .iter()
        .copied()
        .map(FormId)
        .collect();
    state.said = peer
        .campaign
        .dialogue_said
        .iter()
        .copied()
        .map(FormId)
        .collect();
    state.talked_to = peer
        .campaign
        .dialogue_talked_to
        .iter()
        .copied()
        .map(FormId)
        .collect();
    state.reputations = peer
        .campaign
        .reputations
        .iter()
        .map(|(&reputation, &values)| (FormId(reputation), values))
        .collect();

    let applied = with_combat_profile(order, state, &profile, |state| {
        with_coop_player_context(state, peer.player, |state| {
            crate::dialogue::run_coop_line_script(order, scripts, state, speaker, info_id, begin)
        })
    });
    if let Some(profile) = peer.profile.as_mut() {
        update_profile_from_coop_actor_values(profile, peer.player, state);
    }

    PrivateCampaignSnapshot::capture(order, scripts, state, private_quests, private_globals)
        .store_in_ledger(&mut peer.campaign, private_quests, private_globals);
    peer.campaign.reputations = state
        .reputations
        .iter()
        .filter(|(_, values)| **values != (0.0, 0.0))
        .map(|(reputation, values)| (reputation.0, *values))
        .collect();
    peer.campaign.dialogue_topics = state.topics.iter().map(|form| form.0).collect();
    peer.campaign.dialogue_said = state.said.iter().map(|form| form.0).collect();
    peer.campaign.dialogue_talked_to = state.talked_to.iter().map(|form| form.0).collect();

    if peer.campaign.player_items_initialized {
        peer.campaign.player_items = state
            .items
            .iter()
            .filter_map(|(&(owner, item), &count)| {
                (owner == player && count > 0).then_some((item.0, count as u32))
            })
            .collect();
    }

    state.items.retain(|(owner, _), _| *owner != player);
    state.items.extend(
        player_items
            .into_iter()
            .map(|(item, count)| ((player, item), count)),
    );
    state.equipped.remove(&player);
    if let Some(equipment) = player_equipment {
        state.equipped.insert(player, equipment);
    }
    state.topics = player_topics;
    state.said = player_said;
    state.talked_to = player_talked_to;
    state.reputations = player_reputations;
    state.player_female = player_female;
    host_private_campaign.replace_in_state(order, scripts, state, private_quests, private_globals);
    applied
}

fn push_shared_campaign_event(
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

fn publish_host_campaign_changes(
    local_events: &VecDeque<CampaignEvent>,
    published_revision: &mut u32,
    private_quests: &HashSet<FormId>,
    private_globals: &HashSet<FormId>,
    shared_campaign: &mut CampaignLedger,
    shared_revision: &mut u32,
    shared_events: &mut VecDeque<CampaignEvent>,
    peers: &mut HashMap<SocketAddr, Peer>,
) {
    for event in local_events {
        if !sequence_is_newer(event.revision, *published_revision) {
            continue;
        }
        *published_revision = event.revision;
        if campaign_change_is_shared(event.change, private_quests, private_globals)
            && apply_campaign_change_to_ledger(shared_campaign, event.change)
        {
            if let Some(player) = campaign_change_player(event.change) {
                if let Some(peer) = peers.values_mut().find(|peer| peer.player.0 == player) {
                    apply_campaign_change_to_ledger(&mut peer.campaign, event.change);
                    if let CampaignChange::PlayerActorValue {
                        actor_value,
                        current,
                        permanent,
                        ..
                    } = event.change
                    {
                        if let Some(profile) = peer.profile.as_mut() {
                            set_profile_actor_value(profile, actor_value, current, permanent);
                        }
                    }
                }
            }
            push_shared_campaign_event(shared_revision, shared_events, event.change);
        }
    }
}

fn campaign_change_player(change: CampaignChange) -> Option<u8> {
    match change {
        CampaignChange::PlayerActorValue { player, .. }
        | CampaignChange::PlayerMiscStat { player, .. }
        | CampaignChange::PlayerMiscStatsInitialized { player } => Some(player),
        _ => None,
    }
}

fn set_profile_actor_value(
    profile: &mut CombatProfile,
    actor_value: u8,
    current: f32,
    permanent: f32,
) {
    if let Some(entry) = profile
        .actor_values
        .iter_mut()
        .find(|entry| entry.actor_value == actor_value)
    {
        entry.current = current;
        entry.permanent = permanent;
    } else if profile.actor_values.len() < coop_sync_experiment::MAX_PLAYER_PROFILE_ACTOR_VALUES {
        profile.actor_values.push(ActorValueEntry {
            actor_value,
            current,
            permanent,
        });
        profile
            .actor_values
            .sort_unstable_by_key(|entry| entry.actor_value);
    }
}

fn update_profile_from_coop_actor_values(
    profile: &mut CombatProfile,
    player: PlayerId,
    state: &world::scripting::GameState,
) {
    for (&(owner, actor_value), &(current, permanent)) in &state.coop_actor_value_updates {
        if owner == player.0 {
            set_profile_actor_value(profile, actor_value as u8, current, permanent);
        }
    }
}

fn apply_campaign_events(
    ledger: &mut CampaignLedger,
    events: &[CampaignEvent],
    order: &esm::LoadOrder,
    scripts: &world::scripting::ScriptCache,
    schemas: &mut HashMap<u32, Option<CampaignQuestSchema>>,
    sender: PlayerId,
) -> Vec<CampaignEvent> {
    let mut accepted = Vec::new();
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
        if !campaign_change_is_valid(order, scripts, schemas, event.change)
            || !campaign_change_is_allowed_from_client(event.change, sender)
        {
            eprintln!(
                "Rejected invalid co-op campaign event at revision {}",
                event.revision
            );
            // Consume invalid but contiguous events so one malformed report
            // cannot block every later event in this peer's campaign stream.
            ledger.last_revision = Some(event.revision);
            continue;
        }
        apply_campaign_change_to_ledger(ledger, event.change);
        ledger.last_revision = Some(event.revision);
        accepted.push(*event);
    }
    accepted
}

fn campaign_change_is_allowed_from_client(change: CampaignChange, sender: PlayerId) -> bool {
    match change {
        CampaignChange::PlayerActorValue { .. } => false,
        CampaignChange::PlayerMiscStat {
            player,
            authoritative,
            ..
        } => player == sender.0 && !authoritative,
        CampaignChange::PlayerMiscStatsInitialized { player } => player == sender.0,
        _ => true,
    }
}

fn apply_campaign_change_to_ledger(ledger: &mut CampaignLedger, change: CampaignChange) -> bool {
    match change {
        CampaignChange::QuestStage { quest, stage } => {
            if stage == 0 {
                if ledger.stages.remove(&quest).is_none() {
                    return false;
                }
            } else if stage <= ledger.stages.get(&quest).copied().unwrap_or_default() {
                // `Runner::set_stage` stores the maximum reached stage. A
                // delayed or divergent client report cannot move shared
                // quest progress backward.
                return false;
            } else {
                ledger.stages.insert(quest, stage);
            }
        }
        CampaignChange::QuestStageDone { quest, stage, done } => {
            if ledger.stages_done.contains(&(quest, stage)) == done {
                return false;
            }
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
            if ledger.objectives.contains(&(quest, objective)) == completed {
                return false;
            }
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
            let next = (running, completed, failed);
            if ledger.quest_status.get(&quest).copied().unwrap_or_default() == next {
                return false;
            }
            if next == (false, false, false) {
                ledger.quest_status.remove(&quest);
            } else {
                ledger.quest_status.insert(quest, next);
            }
        }
        CampaignChange::Reputation {
            reputation,
            fame,
            infamy,
        } => {
            let next = (fame, infamy);
            if ledger
                .reputations
                .get(&reputation)
                .copied()
                .unwrap_or_default()
                == next
            {
                return false;
            }
            if next == (0.0, 0.0) {
                ledger.reputations.remove(&reputation);
            } else {
                ledger.reputations.insert(reputation, next);
            }
        }
        CampaignChange::QuestVariable {
            quest,
            index,
            value,
        } => {
            let key = (quest, index);
            if value == 0.0 {
                if ledger.quest_variables.remove(&key).is_none() {
                    return false;
                }
            } else if ledger.quest_variables.get(&key) == Some(&value) {
                return false;
            } else {
                ledger.quest_variables.insert(key, value);
            }
        }
        CampaignChange::GlobalValue { global, value } => {
            if ledger.globals.get(&global) == Some(&value) {
                return false;
            }
            ledger.globals.insert(global, value);
        }
        CampaignChange::DialogueFact {
            kind,
            form,
            learned,
        } => {
            let facts = match kind {
                DialogueFactKind::Topic => &mut ledger.dialogue_topics,
                DialogueFactKind::Said => &mut ledger.dialogue_said,
                DialogueFactKind::TalkedTo => &mut ledger.dialogue_talked_to,
            };
            if learned {
                if !facts.insert(form) {
                    return false;
                }
            } else if !facts.remove(&form) {
                return false;
            }
        }
        CampaignChange::PlayerSex { female } => {
            if ledger.player_sex_initialized && ledger.player_female == female {
                return false;
            }
            ledger.player_female = female;
            ledger.player_sex_initialized = true;
        }
        CampaignChange::PlayerItemCount { form, count } => {
            let current = ledger.player_items.get(&form).copied().unwrap_or_default();
            if current == count {
                return false;
            }
            if count == 0 {
                ledger.player_items.remove(&form);
            } else {
                ledger.player_items.insert(form, count);
            }
        }
        CampaignChange::PlayerItemsInitialized => {
            if ledger.player_items_initialized {
                return false;
            }
            ledger.player_items_initialized = true;
        }
        CampaignChange::PlayerActorValue {
            player,
            actor_value,
            current,
            permanent,
        } => {
            let key = (player, actor_value);
            let next = (current, permanent);
            if ledger.player_actor_values.get(&key) == Some(&next) {
                return false;
            }
            ledger.player_actor_values.insert(key, next);
        }
        CampaignChange::PlayerMiscStat {
            player,
            stat,
            value,
            ..
        } => {
            let key = (player, stat);
            if ledger
                .player_misc_stats
                .get(&key)
                .copied()
                .unwrap_or_default()
                == value
            {
                return false;
            }
            if value == 0 {
                ledger.player_misc_stats.remove(&key);
            } else {
                ledger.player_misc_stats.insert(key, value);
            }
        }
        CampaignChange::PlayerMiscStatsInitialized { player } => {
            if !ledger.player_misc_stats_initialized.insert(player) {
                return false;
            }
        }
    }
    true
}

fn campaign_change_would_update(ledger: &CampaignLedger, change: CampaignChange) -> bool {
    match change {
        CampaignChange::QuestStage { quest, stage } => {
            if stage == 0 {
                false
            } else {
                stage > ledger.stages.get(&quest).copied().unwrap_or_default()
            }
        }
        CampaignChange::QuestStageDone { quest, stage, done } => {
            ledger.stages_done.contains(&(quest, stage)) != done
        }
        CampaignChange::QuestObjective {
            quest,
            objective,
            completed,
        } => ledger.objectives.contains(&(quest, objective)) != completed,
        CampaignChange::QuestStatus {
            quest,
            running,
            completed,
            failed,
        } => {
            ledger.quest_status.get(&quest).copied().unwrap_or_default()
                != (running, completed, failed)
        }
        CampaignChange::QuestVariable {
            quest,
            index,
            value,
        } => {
            ledger
                .quest_variables
                .get(&(quest, index))
                .copied()
                .unwrap_or(0.0)
                != value
        }
        CampaignChange::GlobalValue { global, value } => {
            ledger.globals.get(&global).copied() != Some(value)
        }
        CampaignChange::DialogueFact {
            kind,
            form,
            learned,
        } => {
            let facts = match kind {
                DialogueFactKind::Topic => &ledger.dialogue_topics,
                DialogueFactKind::Said => &ledger.dialogue_said,
                DialogueFactKind::TalkedTo => &ledger.dialogue_talked_to,
            };
            facts.contains(&form) != learned
        }
        CampaignChange::PlayerSex { female } => {
            !ledger.player_sex_initialized || ledger.player_female != female
        }
        CampaignChange::PlayerItemCount { form, count } => {
            ledger.player_items.get(&form).copied().unwrap_or_default() != count
        }
        CampaignChange::PlayerItemsInitialized => !ledger.player_items_initialized,
        CampaignChange::PlayerActorValue {
            player,
            actor_value,
            current,
            permanent,
        } => ledger.player_actor_values.get(&(player, actor_value)) != Some(&(current, permanent)),
        CampaignChange::PlayerMiscStat {
            player,
            stat,
            value,
            ..
        } => {
            ledger
                .player_misc_stats
                .get(&(player, stat))
                .copied()
                .unwrap_or_default()
                != value
        }
        CampaignChange::PlayerMiscStatsInitialized { player } => {
            !ledger.player_misc_stats_initialized.contains(&player)
        }
        CampaignChange::Reputation { .. } => false,
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
            WorldChange::ActorDied { reference, .. } | WorldChange::ActorRevived { reference } => {
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
        WorldChange::ActorDied { reference, .. } | WorldChange::ActorRevived { reference } => {
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
        WorldChange::ActorDied { player, .. } => {
            state.dead.contains(&FormId(reference))
                && state.coop_killers.get(&FormId(reference)).copied() == player
        }
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
        WorldChange::ActorDied { reference, player } => {
            let inserted = state.dead.insert(FormId(reference));
            if let Some(player) = player {
                state.coop_killers.insert(FormId(reference), player);
            } else {
                state.coop_killers.remove(&FormId(reference));
            }
            inserted
        }
        WorldChange::ActorRevived { reference } => {
            state.coop_killers.remove(&FormId(reference));
            state.dead.remove(&FormId(reference))
        }
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
    confirmed_killers: &mut HashMap<u32, u8>,
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
                WorldChange::ActorDied { reference, .. }
                | WorldChange::ActorRevived { reference }
                | WorldChange::ReferenceDestroyed { reference }
                | WorldChange::ReferenceRestored { reference }
                | WorldChange::ActorHealth { reference, .. } => reference,
            });
            match event.change {
                WorldChange::ActorDied { player, .. } => {
                    confirmed_dead.insert(reference.0);
                    if let Some(player) = player {
                        confirmed_killers.insert(reference.0, player);
                    } else {
                        confirmed_killers.remove(&reference.0);
                    }
                }
                WorldChange::ActorRevived { .. } => {
                    confirmed_dead.remove(&reference.0);
                    confirmed_killers.remove(&reference.0);
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
    confirmed_killers: &HashMap<u32, u8>,
    pending: &VecDeque<WorldEvent>,
) {
    sync_confirmed_world(
        state,
        confirmed_dead,
        confirmed_destroyed,
        confirmed_killers,
    );
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
    confirmed_killers: &HashMap<u32, u8>,
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
    state.coop_killers = confirmed_killers
        .iter()
        .map(|(&reference, &player)| (esm::FormId(reference), player))
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
