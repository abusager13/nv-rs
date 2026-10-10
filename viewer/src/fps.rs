//! A small on-screen frame-rate readout.

use bevy::prelude::*;

#[derive(Component)]
pub(crate) struct Counter;

#[derive(Resource, Default)]
pub struct Readout {
    elapsed: f32,
    frames: u32,
    fps: f32,
}

pub fn setup(mut commands: Commands) {
    commands.spawn((
        Text::new("FPS --"),
        TextFont {
            font_size: 14.0,
            ..default()
        },
        TextColor(Color::srgb(0.95, 0.9, 0.72)),
        Node {
            position_type: PositionType::Absolute,
            top: Val::Px(8.0),
            right: Val::Px(10.0),
            padding: UiRect::axes(Val::Px(8.0), Val::Px(4.0)),
            ..default()
        },
        BackgroundColor(Color::srgba(0.0, 0.0, 0.0, 0.55)),
        Counter,
    ));
}

pub(crate) fn update(
    time: Res<Time<Real>>,
    mut readout: ResMut<Readout>,
    mut text: Query<&mut Text, With<Counter>>,
) {
    readout.elapsed += time.delta_secs();
    readout.frames += 1;
    if readout.elapsed >= 0.5 {
        readout.fps = readout.frames as f32 / readout.elapsed;
        readout.elapsed = 0.0;
        readout.frames = 0;
    }
    for mut text in &mut text {
        text.0 = format!("FPS {:.0}", readout.fps);
    }
}
