//! The order of the viewer's per-frame systems, from the game's frame
//! (`Main::OnIdle`, Xbox PDB; PC `0086e650`; `world::frame`,
//! docs/FRAME_SKELETON.md "PR 3 result").
//!
//! Each of `world::frame`'s stages is a system set ([`FrameSet::Stage`]),
//! run in the exe's order ([`world::frame::stages`]), and so is each of its
//! steps ([`FrameSet::Step`], an index into `world::frame::STEPS`, inside
//! its stage, in call order). A system that is (part of) a step goes in
//! the step's set; one that belongs to a stage without being one step goes
//! in the stage's set. Each set runs under its gate from `world::frame`
//! ([`world::frame::step_runs`], [`world::frame::stage_reached`]),
//! evaluated on one [`FrameState`] ([`ThisFrame`]) filled once per frame
//! by [`begin_frame`], the first system of the first stage.
//!
//! Two sets run ahead of the stages and one after them
//! ([`ViewerSet`]); see there for what they hold and why.

use bevy::prelude::*;
use world::frame::{self, FrameState, Stage, STEPS};

/// The game's frame as system sets.
#[derive(SystemSet, Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum FrameSet {
    /// One of `world::frame`'s stages.
    Stage(Stage),
    /// One of its steps (an index into `world::frame::STEPS`).
    Step(usize),
}

impl FrameSet {
    /// The set of the first step that calls `address`.
    pub fn step(address: u32) -> FrameSet {
        FrameSet::Step(frame::step_of(address).expect("a call of Main::OnIdle"))
    }
}

/// `Main::OnIdle_UpdateTimer` (step 32).
pub const UPDATE_TIMER: u32 = 0x0086_f260;
/// `Main::OnIdle_HandleMenuBackground` (step 44).
pub const HANDLE_MENU_BACKGROUND: u32 = 0x0086_f450;
/// `BSTreeManager::Update` (step 77).
pub const TREE_MANAGER_UPDATE: u32 = 0x0066_52e0;

/// Viewer work outside the frame's stages.
#[derive(SystemSet, Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ViewerSet {
    /// First: putting a newly loaded interior or exterior on screen and
    /// streaming the land around the player. In the exe, loading belongs to
    /// the frame (`TES::ShowLoadingMenu`, step 39; the grid cells' attach
    /// in `Main::OnIdle_UpdateCurrentGridCell`, step 78), but the viewer's
    /// loaders aren't split along those calls yet and other systems rely on
    /// a scene being installed at the start of the frame (a door or a
    /// script's `MoveTo` asks for it one frame, it is in place the next).
    Loading,
    /// Then the interface: the game's menus, the viewer's own menus,
    /// V.A.T.S. and the lockpicking menu, which take Bevy's input
    /// (`reset_all`) before the player's systems read it. In the exe this is
    /// `Interface::Idle` (through `Main::OnIdle_DoInterfaceIdle`), in stage 5
    /// with one thread or on the AI thread in stage 6; the player's update
    /// (stage 2) reads the controls `Main::OnIdle_PollControls` polled in the
    /// previous frame (step 33), after that frame's interface idle, so there
    /// too the interface meets the input before the player does. It moves
    /// into its stage once the player's update tests menu mode itself
    /// (Phase 1 PR 4 and PR 7).
    Interface,
    /// Last: what is not in `Main::OnIdle`, the viewer's own tools
    /// (screenshots, help, the cursor, exposure, the F12 report, the frame
    /// rate, the window's focus, the present mode, the console key's switch
    /// to its free camera, billboards turned to Bevy's camera); and output
    /// whose place in the frame isn't traced, which only needs the frame's
    /// work done (the GPU's grass and water around the camera, sounds, music
    /// and the radio played).
    AfterFrame,
}

/// The frame's inputs to the gates, filled once per frame ([`begin_frame`]).
#[derive(Resource, Debug, Clone, Copy, PartialEq)]
pub struct ThisFrame(pub FrameState);

impl Default for ThisFrame {
    fn default() -> ThisFrame {
        ThisFrame(viewer_frame_state(FrameInputs::default()))
    }
}

/// The values the viewer has for the frame's inputs.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct FrameInputs {
    /// Tab and Alt held.
    pub alt_tab_held: bool,
    /// A menu is up: the game's own, the viewer's, the Pip-Boy, the
    /// lockpicking menu (`menus::Menus::is_open`), the dialogue menu, the
    /// V.A.T.S. menu.
    pub menu_up: bool,
    /// The V.A.T.S. manager's mode (`vats::Vats::manager_mode`).
    pub vats_mode: u8,
    /// The class of the top game menu on screen (`game_menus::MenuDraw`).
    pub top_menu: Option<i32>,
}

/// The thread count `main` leaves (`0086a950`-`0086a990`): the processor
/// count (`GetSystemInfo`; here the standard library's), raised to 2 when
/// it is 1. Whether the INI can lower it afterwards is not traced.
pub fn thread_count() -> i32 {
    let n = std::thread::available_parallelism().map_or(1, |n| n.get());
    i32::try_from(n).unwrap_or(i32::MAX).max(2)
}

/// The frame's inputs from the viewer's values. Those the viewer has no
/// source for are fixed at the normal PC case (docs/FRAME_SKELETON.md, "PR 3
/// result"):
///
/// - fader 1 (`FaderManager::IsFaderVisible(1)`): not visible. The viewer's
///   two faders (the sleep's and the menus' fade to black) are fader 0.
/// - the frozen world (`Main` +7, the console's `TFC` 1, 2 or 5): not
///   frozen. The viewer has no console window, and its free camera (the `
///   key, `walk::toggle_walking`) is its own, not `TFC`: the world runs.
/// - the interface mode (`[011d8a80]`+0xc): 1, game mode. Menu mode comes
///   from `menu_up`; only the memory free's gate (step 13, open) reads the
///   number itself.
/// - the thread count: [`thread_count`], at least 2 as `main` leaves it.
///
/// Also fixed: the console is hidden (no console window), the Pip-Boy's
/// opening is part of `menu_up` (`Menus::pipboy`), and the requests and
/// loading inputs of steps no viewer system implements keep
/// `FrameState::default`'s values.
pub fn viewer_frame_state(v: FrameInputs) -> FrameState {
    FrameState {
        alt_tab_held: v.alt_tab_held,
        in_menu_mode: v.menu_up,
        pipboy_opening: false,
        fader_visible: false,
        console_visible: false,
        interface_mode: 1,
        world_frozen: false,
        threads: thread_count(),
        vats_mode: u32::from(v.vats_mode),
        top_menu: v.top_menu.and_then(|c| u32::try_from(c).ok()).unwrap_or(0),
        ..FrameState::default()
    }
}

/// Fills [`ThisFrame`]: the menu state queries at the start of
/// `Main::OnIdle` (steps 3-12), with the Tab and Alt test (`0086e682`,
/// `0086e69a`: `GetAsyncKeyState` on Tab and either Alt).
#[allow(clippy::too_many_arguments)]
pub fn begin_frame(
    keys: Res<ButtonInput<KeyCode>>,
    menus: Res<crate::menus::Menus>,
    conversation: Res<crate::dialogue::Conversation>,
    vats: Option<Res<crate::vats::Vats>>,
    draw: Res<crate::game_menus::MenuDraw>,
    mut this: ResMut<ThisFrame>,
) {
    let vats_mode = vats.as_ref().map_or(0, |v| v.manager_mode());
    let in_dialogue = conversation.0.as_ref().is_some_and(|t| !t.is_line_only());
    this.0 = viewer_frame_state(FrameInputs {
        alt_tab_held: keys.pressed(KeyCode::Tab)
            && (keys.pressed(KeyCode::AltLeft) || keys.pressed(KeyCode::AltRight)),
        menu_up: menus.is_open() || in_dialogue || vats.is_some_and(|v| v.in_menu()),
        vats_mode,
        top_menu: draw.1.last().copied(),
    });
}

/// Configures the sets in `Update`: the viewer's two sets ahead, the
/// stages in the exe's order, each step inside its stage in call order,
/// the viewer's last set after; each stage and step under its gate on
/// [`ThisFrame`].
pub fn configure(app: &mut App) {
    app.init_resource::<ThisFrame>();
    let stages = frame::stages();
    app.configure_sets(
        Update,
        (
            ViewerSet::Loading,
            ViewerSet::Interface,
            FrameSet::Stage(stages[0]),
        )
            .chain(),
    );
    for pair in stages.windows(2) {
        app.configure_sets(
            Update,
            FrameSet::Stage(pair[0]).before(FrameSet::Stage(pair[1])),
        );
    }
    app.configure_sets(
        Update,
        FrameSet::Stage(*stages.last().expect("stages")).before(ViewerSet::AfterFrame),
    );
    for stage in stages {
        app.configure_sets(
            Update,
            FrameSet::Stage(stage)
                .run_if(move |this: Res<ThisFrame>| frame::stage_reached(&this.0, stage)),
        );
    }
    for (i, step) in STEPS.iter().enumerate() {
        app.configure_sets(
            Update,
            FrameSet::Step(i)
                .in_set(FrameSet::Stage(step.stage))
                .run_if(move |this: Res<ThisFrame>| frame::step_runs(&this.0, i)),
        );
        if i + 1 < STEPS.len() && STEPS[i + 1].stage == step.stage {
            app.configure_sets(Update, FrameSet::Step(i).before(FrameSet::Step(i + 1)));
        }
    }
}

/// The frame's sets and [`begin_frame`].
pub struct FrameOrderPlugin;

impl Plugin for FrameOrderPlugin {
    fn build(&self, app: &mut App) {
        configure(app);
        app.add_systems(
            Update,
            begin_frame.in_set(FrameSet::Stage(Stage::FrameStart)),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// What ran, in order.
    #[derive(Resource, Default)]
    struct Ran(Vec<&'static str>);

    fn note(name: &'static str) -> impl FnMut(ResMut<Ran>) {
        move |mut ran: ResMut<Ran>| ran.0.push(name)
    }

    fn stage_name(stage: Stage) -> &'static str {
        match stage {
            Stage::FrameStart => "FrameStart",
            Stage::Player => "Player",
            Stage::Housekeeping => "Housekeeping",
            Stage::WorldAndTime => "WorldAndTime",
            Stage::InterfaceAndScene => "InterfaceAndScene",
            Stage::AiStart => "AiStart",
            Stage::Render => "Render",
            Stage::AiJoin => "AiJoin",
        }
    }

    /// An app with the sets and nothing else; the state is set by hand.
    fn frame_app(state: FrameState) -> App {
        let mut app = App::new();
        app.add_plugins(MinimalPlugins);
        configure(&mut app);
        app.insert_resource(ThisFrame(state)).init_resource::<Ran>();
        app
    }

    /// The configured order of the sets is `world::frame`'s stage order,
    /// with the viewer's sets ahead and after; and steps run in call order
    /// inside their stage. Systems are added in reverse so that only the
    /// sets can give the order.
    #[test]
    fn sets_run_in_the_frames_order() {
        let mut app = frame_app(FrameState::default());
        app.add_systems(Update, note("AfterFrame").in_set(ViewerSet::AfterFrame));
        for stage in frame::stages().into_iter().rev() {
            app.add_systems(
                Update,
                note(stage_name(stage)).in_set(FrameSet::Stage(stage)),
            );
        }
        app.add_systems(Update, note("Interface").in_set(ViewerSet::Interface))
            .add_systems(Update, note("Loading").in_set(ViewerSet::Loading));
        app.update();
        let mut want = vec!["Loading", "Interface"];
        want.extend(frame::stages().into_iter().map(stage_name));
        want.push("AfterFrame");
        assert_eq!(app.world().resource::<Ran>().0, want);
        // The exe's stage order, as STEPS reaches the stages.
        let mut from_steps: Vec<Stage> = STEPS.iter().map(|s| s.stage).collect();
        from_steps.dedup();
        assert_eq!(frame::stages(), from_steps);

        // Steps: the tree manager (77) after the menu background (44)
        // after the timer (32), whatever order they're added in, and each
        // with its stage's other systems.
        let mut app = frame_app(FrameState::default());
        app.add_systems(
            Update,
            note("77").in_set(FrameSet::step(TREE_MANAGER_UPDATE)),
        )
        .add_systems(
            Update,
            note("44").in_set(FrameSet::step(HANDLE_MENU_BACKGROUND)),
        )
        .add_systems(Update, note("32").in_set(FrameSet::step(UPDATE_TIMER)))
        .add_systems(
            Update,
            note("Player").in_set(FrameSet::Stage(Stage::Player)),
        );
        app.update();
        assert_eq!(
            app.world().resource::<Ran>().0,
            ["Player", "32", "44", "77"]
        );
    }

    /// A system in a gated set doesn't run while its gate is closed: menu
    /// mode stops `Calendar::Update` (step 56, the world runs), not the rest
    /// of its stage; Tab and Alt held stop every stage after the first.
    #[test]
    fn gates_stop_their_sets() {
        let calendar = FrameSet::step(0x0086_7a40);
        let add = |app: &mut App| {
            app.add_systems(
                Update,
                note("start").in_set(FrameSet::Stage(Stage::FrameStart)),
            )
            .add_systems(Update, note("calendar").in_set(calendar))
            .add_systems(
                Update,
                note("world")
                    .in_set(FrameSet::Stage(Stage::WorldAndTime))
                    .after(calendar),
            )
            .add_systems(Update, note("after").in_set(ViewerSet::AfterFrame));
        };
        let game = viewer_frame_state(FrameInputs::default());

        let mut a = frame_app(game);
        add(&mut a);
        a.update();
        assert_eq!(
            a.world().resource::<Ran>().0,
            ["start", "calendar", "world", "after"]
        );

        let menu = viewer_frame_state(FrameInputs {
            menu_up: true,
            ..FrameInputs::default()
        });
        let mut a = frame_app(menu);
        add(&mut a);
        a.update();
        assert_eq!(a.world().resource::<Ran>().0, ["start", "world", "after"]);

        let held = viewer_frame_state(FrameInputs {
            alt_tab_held: true,
            ..FrameInputs::default()
        });
        let mut a = frame_app(held);
        add(&mut a);
        a.update();
        assert_eq!(a.world().resource::<Ran>().0, ["start", "after"]);
    }

    /// The fixed inputs: fader 1 hidden, the world not frozen, game mode's
    /// interface mode, two threads or more; V.A.T.S. playback reaches the
    /// field-of-view gate.
    #[test]
    fn fixed_inputs() {
        let s = viewer_frame_state(FrameInputs::default());
        assert!(!s.fader_visible && !s.world_frozen && !s.console_visible);
        assert_eq!(s.interface_mode, 1);
        assert!(s.threads >= 2);
        assert!(s.world_runs() && s.process_lists());
        let playback = viewer_frame_state(FrameInputs {
            vats_mode: world::vats::mode::PLAYBACK,
            ..FrameInputs::default()
        });
        assert!(!frame::Gate::NotVatsPlayback.holds(&playback));
        let sleeping = viewer_frame_state(FrameInputs {
            top_menu: Some(1012),
            ..FrameInputs::default()
        });
        assert!(frame::Gate::SleepWaitMenuTop.holds(&sleeping));
    }
}
