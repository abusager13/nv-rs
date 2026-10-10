//! Opt-in loadout for hands-on controller checks.

use bevy::prelude::Resource;
use esm::{sig, FormId, LoadOrder};
use world::{combat, dialogue::PLAYER_REF, scripting::GameState};

#[derive(Resource, Default)]
pub struct Loadout(pub bool);

#[derive(Resource, Default)]
pub struct Applied(pub bool);

/// Make the current in-memory session convenient for combat input checks.
/// This is only called with `--controller-test`; it does not touch game files.
pub fn apply(order: &LoadOrder, state: &mut GameState) -> (usize, usize) {
    const CARRY_WEIGHT: u16 = 13;
    const AMMO_PER_TYPE: i32 = 1_000;
    const TEST_HEALTH: f64 = 1_000_000.0;
    const TEST_CARRY_WEIGHT: f64 = 1_000_000.0;

    state
        .actor_values
        .insert((PLAYER_REF, combat::av::HEALTH), TEST_HEALTH);
    state
        .actor_values
        .insert((PLAYER_REF, CARRY_WEIGHT), TEST_CARRY_WEIGHT);
    state.stock(order, PLAYER_REF);

    let weapons: Vec<FormId> = order
        .records_of_type(sig::WEAP)
        .map(|record| record.form_id)
        .filter(|&id| combat::Weapon::load(order, id).is_some())
        .collect();
    for &weapon in &weapons {
        *state.items.entry((PLAYER_REF, weapon)).or_insert(0) += 1;
        state.added(order, PLAYER_REF, weapon, 1);
    }

    let ammo: Vec<FormId> = order
        .records_of_type(sig::AMMO)
        .map(|record| record.form_id)
        .collect();
    for item in &ammo {
        *state.items.entry((PLAYER_REF, *item)).or_insert(0) += AMMO_PER_TYPE;
    }

    (weapons.len(), ammo.len())
}

/// Keep the opt-in test player alive by healing health damage once per
/// frame. The large base-health value also protects against a lethal hit
/// before this frame's world update.
pub fn keep_alive(state: &mut GameState) {
    state.damage.remove(&PLAYER_REF);
    state.dead.remove(&PLAYER_REF);
    state.deaths.remove(&PLAYER_REF);
    state.more.down.remove(&PLAYER_REF);
}
