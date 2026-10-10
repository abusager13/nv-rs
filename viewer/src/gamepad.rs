//! Standard gamepad input for the viewer.
//!
//! Bevy supplies connected pads through its platform-specific gamepad
//! backends. Keep a frame snapshot here so gameplay, menus, and the Pip-Boy
//! share the same primary pad and button edge detection.

use bevy::{
    input::gamepad::{GamepadAxisChangedEvent, GamepadButtonChangedEvent},
    prelude::*,
};

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
    shape_stick(raw)
}

fn shape_stick(raw: Vec2) -> Vec2 {
    let magnitude = raw.length();
    if magnitude <= DEAD_ZONE {
        Vec2::ZERO
    } else {
        raw.normalize() * ((magnitude - DEAD_ZONE) / (1.0 - DEAD_ZONE)).min(1.0)
    }
}

#[cfg(target_os = "macos")]
fn native_game_controller() -> Option<(Vec2, Vec2, u32)> {
    use objc2_game_controller::{GCController, GCControllerButtonInput};

    fn set_button(held: &mut u32, button: GamepadButton, down: bool) {
        if down {
            *held |= bit(button);
        }
    }

    fn pressed(button: impl std::ops::Deref<Target = GCControllerButtonInput>) -> bool {
        // GameController exposes these input reads as Objective-C messages.
        unsafe { button.isPressed() }
    }

    // Read Apple's standardized extended-gamepad profile on macOS. The shared
    // input snapshot below keeps button edges and stick shaping consistent
    // with the Bevy/Gilrs path used on other platforms.
    let controllers = unsafe { GCController::controllers() };
    let controller = controllers.firstObject()?;
    let gamepad = unsafe { controller.extendedGamepad() }?;

    let left = unsafe { gamepad.leftThumbstick() };
    let right = unsafe { gamepad.rightThumbstick() };
    let left_stick = shape_stick(Vec2::new(unsafe { left.xAxis().value() }, unsafe {
        left.yAxis().value()
    }));
    let right_stick = shape_stick(Vec2::new(unsafe { right.xAxis().value() }, unsafe {
        right.yAxis().value()
    }));

    let mut held = 0;
    set_button(
        &mut held,
        GamepadButton::South,
        pressed(unsafe { gamepad.buttonA() }),
    );
    set_button(
        &mut held,
        GamepadButton::East,
        pressed(unsafe { gamepad.buttonB() }),
    );
    set_button(
        &mut held,
        GamepadButton::West,
        pressed(unsafe { gamepad.buttonX() }),
    );
    set_button(
        &mut held,
        GamepadButton::North,
        pressed(unsafe { gamepad.buttonY() }),
    );
    set_button(
        &mut held,
        GamepadButton::Start,
        pressed(unsafe { gamepad.buttonMenu() }),
    );
    set_button(
        &mut held,
        GamepadButton::Select,
        unsafe { gamepad.buttonOptions() }.is_some_and(pressed),
    );
    set_button(
        &mut held,
        GamepadButton::Mode,
        unsafe { gamepad.buttonHome() }.is_some_and(pressed),
    );
    set_button(
        &mut held,
        GamepadButton::LeftTrigger,
        pressed(unsafe { gamepad.leftShoulder() }),
    );
    set_button(
        &mut held,
        GamepadButton::RightTrigger,
        pressed(unsafe { gamepad.rightShoulder() }),
    );
    set_button(
        &mut held,
        GamepadButton::LeftTrigger2,
        unsafe { gamepad.leftTrigger().value() } >= 0.25,
    );
    set_button(
        &mut held,
        GamepadButton::RightTrigger2,
        unsafe { gamepad.rightTrigger().value() } >= 0.25,
    );
    set_button(
        &mut held,
        GamepadButton::LeftThumb,
        unsafe { gamepad.leftThumbstickButton() }.is_some_and(pressed),
    );
    set_button(
        &mut held,
        GamepadButton::RightThumb,
        unsafe { gamepad.rightThumbstickButton() }.is_some_and(pressed),
    );

    let dpad = unsafe { gamepad.dpad() };
    for (button, down) in [
        (GamepadButton::DPadUp, pressed(unsafe { dpad.up() })),
        (GamepadButton::DPadDown, pressed(unsafe { dpad.down() })),
        (GamepadButton::DPadLeft, pressed(unsafe { dpad.left() })),
        (GamepadButton::DPadRight, pressed(unsafe { dpad.right() })),
    ] {
        set_button(&mut held, button, down);
    }

    Some((left_stick, right_stick, held))
}

fn update_input(input: &mut Input, left_stick: Vec2, right_stick: Vec2, held: u32, debug: bool) {
    input.left_stick_just_pressed =
        input.left_stick.length_squared() <= 0.01 && left_stick.length_squared() > 0.01;
    let pressed = held & !input.held;
    let released = input.held & !held;
    if debug {
        for button in GamepadButton::all() {
            if pressed & bit(button) != 0 {
                println!("Gamepad button pressed: {button:?}.");
            }
            if released & bit(button) != 0 {
                println!("Gamepad button released: {button:?}.");
            }
        }
        if (left_stick - input.left_stick).length_squared() > 0.04
            || (right_stick - input.right_stick).length_squared() > 0.04
        {
            println!("Gamepad sticks: left={left_stick:?} right={right_stick:?}.");
        }
    }
    input.connected = true;
    input.left_stick = left_stick;
    input.right_stick = right_stick;
    input.pressed = pressed;
    input.released = released;
    input.held = held;
}

/// Snapshot the first connected pad once per frame. The dead zone removes
/// stick drift while keeping a smooth range for walking and looking.
pub fn poll(
    mut input: ResMut<Input>,
    pads: Query<&Gamepad>,
    mut button_events: EventReader<GamepadButtonChangedEvent>,
    mut axis_events: EventReader<GamepadAxisChangedEvent>,
) {
    let debug = std::env::var_os("NV_GAMEPAD_DEBUG").is_some();
    if debug {
        for event in button_events.read() {
            println!(
                "Processed gamepad button event: button={:?} state={:?} value={:.3}.",
                event.button, event.state, event.value
            );
        }
        for event in axis_events.read() {
            println!(
                "Processed gamepad axis event: axis={:?} value={:.3}.",
                event.axis, event.value
            );
        }
    } else {
        button_events.clear();
        axis_events.clear();
    }

    #[cfg(target_os = "macos")]
    if let Some((left_stick, right_stick, held)) = native_game_controller() {
        if debug && !input.connected {
            println!("Using Apple's native Game Controller input.");
        }
        update_input(&mut input, left_stick, right_stick, held, debug);
        return;
    }

    let Some(pad) = pads.iter().next() else {
        if debug && input.connected {
            println!("Gamepad disconnected.");
        }
        input.connected = false;
        input.left_stick = Vec2::ZERO;
        input.right_stick = Vec2::ZERO;
        input.left_stick_just_pressed = false;
        input.pressed = 0;
        input.released = input.held;
        input.held = 0;
        return;
    };

    if debug && !input.connected {
        println!(
            "Gamepad connected: vendor={:?} product={:?}.",
            pad.vendor_id(),
            pad.product_id()
        );
    }

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
    let right_stick = stick(pad, GamepadAxis::RightStickX, GamepadAxis::RightStickY);
    update_input(&mut input, left_stick, right_stick, held, debug);
}
