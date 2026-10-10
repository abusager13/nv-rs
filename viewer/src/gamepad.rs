//! Standard gamepad input for the viewer.
//!
//! Bevy supplies connected pads through its platform-specific gamepad
//! backends. Keep a frame snapshot here so gameplay, menus, and the Pip-Boy
//! share the same primary pad and button edge detection.

use bevy::prelude::*;

const DEAD_ZONE: f32 = 0.18;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Binding {
    Button(GamepadButton),
}

impl Binding {
    /// The standard Xbox-style button codes stored in FalloutPrefs.ini.
    pub fn from_legacy(code: u8) -> Option<Self> {
        use GamepadButton as B;
        Some(Self::Button(match code {
            0x01 => B::DPadUp,
            0x07 => B::DPadDown,
            0x08 => B::LeftThumb,
            0x09 => B::RightThumb,
            0x0a => B::South,
            0x0b => B::Start,
            0x0c => B::West,
            0x0d => B::North,
            0x0e => B::East,
            0x0f => B::Select,
            0x10 => B::LeftTrigger2,
            0x11 => B::RightTrigger2,
            _ => return None,
        }))
    }
}

#[derive(Resource, Debug, Default)]
pub struct Input {
    pub connected: bool,
    pub left_stick: Vec2,
    pub right_stick: Vec2,
    pub left_stick_just_pressed: bool,
    held: u32,
    pressed: u32,
    released: u32,
}

impl Input {
    pub fn held(&self, button: GamepadButton) -> bool {
        self.held & bit(button) != 0
    }

    pub fn just_pressed(&self, button: GamepadButton) -> bool {
        self.pressed & bit(button) != 0
    }

    pub fn just_released(&self, button: GamepadButton) -> bool {
        self.released & bit(button) != 0
    }

    pub fn binding_held(&self, binding: Option<Binding>) -> bool {
        binding.is_some_and(|Binding::Button(button)| self.held(button))
    }

    pub fn binding_just_pressed(&self, binding: Option<Binding>) -> bool {
        binding.is_some_and(|Binding::Button(button)| self.just_pressed(button))
    }

    pub fn binding_just_released(&self, binding: Option<Binding>) -> bool {
        binding.is_some_and(|Binding::Button(button)| self.just_released(button))
    }
}

fn bit(button: GamepadButton) -> u32 {
    1u32 << GamepadButton::all()
        .iter()
        .position(|candidate| *candidate == button)
        .expect("standard gamepad button")
}

fn stick(pad: &Gamepad, x: GamepadAxis, y: GamepadAxis) -> Vec2 {
    let raw = Vec2::new(pad.get(x).unwrap_or(0.0), -pad.get(y).unwrap_or(0.0));
    let magnitude = raw.length();
    if magnitude <= DEAD_ZONE {
        Vec2::ZERO
    } else {
        raw.normalize() * ((magnitude - DEAD_ZONE) / (1.0 - DEAD_ZONE)).min(1.0)
    }
}

/// Snapshot the first connected pad once per frame. The dead zone removes
/// stick drift while keeping a smooth range for walking and looking.
pub fn poll(mut input: ResMut<Input>, pads: Query<&Gamepad>) {
    let Some(pad) = pads.iter().next() else {
        input.connected = false;
        input.left_stick = Vec2::ZERO;
        input.right_stick = Vec2::ZERO;
        input.left_stick_just_pressed = false;
        input.pressed = 0;
        input.released = input.held;
        input.held = 0;
        return;
    };

    let mut held = 0;
    for button in GamepadButton::all() {
        let trigger = matches!(
            button,
            GamepadButton::LeftTrigger
                | GamepadButton::LeftTrigger2
                | GamepadButton::RightTrigger
                | GamepadButton::RightTrigger2
        );
        let down = if trigger {
            pad.get(button).is_some_and(|value| value >= 0.25) || pad.pressed(button)
        } else {
            pad.pressed(button)
        };
        if down {
            held |= bit(button);
        }
    }
    let left_stick = stick(pad, GamepadAxis::LeftStickX, GamepadAxis::LeftStickY);
    input.left_stick_just_pressed =
        input.left_stick.length_squared() <= 0.01 && left_stick.length_squared() > 0.01;
    input.connected = true;
    input.left_stick = left_stick;
    input.right_stick = stick(pad, GamepadAxis::RightStickX, GamepadAxis::RightStickY);
    input.pressed = held & !input.held;
    input.released = input.held & !held;
    input.held = held;
}
