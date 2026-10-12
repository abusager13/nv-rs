//! Compare route checkpoints captured as original `.fos` saves and nv-rs's
//! own F12 `state.txt` files. The original saves are decoded by `fos` and
//! `world::fos_import`; neither input is modified.

use std::collections::BTreeSet;
use std::io::Write;
use std::path::{Path, PathBuf};

use esm::{ActivePlugins, FormId, LoadOrder};
use fos::{decode, RefId, Save};
use world::dialogue::PLAYER_REF;
use world::scripting::GameState;

use crate::CliError;

struct Checkpoint {
    name: String,
    original: PathBuf,
    viewer: PathBuf,
    quests: Vec<FormId>,
    actors: Vec<FormId>,
}

fn file_error(path: &Path, message: impl ToString) -> CliError {
    CliError::InFile {
        path: path.display().to_string(),
        message: message.to_string(),
    }
}

fn read(path: &Path) -> Result<Vec<u8>, CliError> {
    std::fs::read(path).map_err(|e| CliError::Open {
        path: path.display().to_string(),
        message: e.to_string(),
    })
}

fn resolve(order: &LoadOrder, token: &str) -> Result<FormId, CliError> {
    if token.eq_ignore_ascii_case("player") {
        return Ok(PLAYER_REF);
    }
    let hex = token.strip_prefix("0x").unwrap_or(token);
    if let Ok(id) = u32::from_str_radix(hex, 16) {
        if token.starts_with("0x") || token.len() == 8 {
            return Ok(FormId(id));
        }
    }
    order
        .find_by_editor_id(token, None)?
        .map(|record| record.form_id)
        .ok_or_else(|| CliError::NotFound(format!("form '{token}' is not in the Data folder")))
}

fn ids(order: &LoadOrder, value: &str) -> Result<Vec<FormId>, CliError> {
    value
        .split(',')
        .filter(|token| !token.trim().is_empty())
        .map(|token| resolve(order, token.trim()))
        .collect()
}

fn checkpoints(order: &LoadOrder, path: &Path) -> Result<Vec<Checkpoint>, CliError> {
    let bytes = read(path)?;
    let text = String::from_utf8(bytes).map_err(|e| file_error(path, e))?;
    let base = path.parent().unwrap_or_else(|| Path::new("."));
    let mut seen = BTreeSet::new();
    let mut rows = Vec::new();
    for (i, line) in text.lines().enumerate() {
        if line.trim().is_empty() || line.starts_with('#') {
            continue;
        }
        let columns: Vec<_> = line.split('\t').collect();
        if columns.len() != 5 || columns[..3].iter().any(|part| part.trim().is_empty()) {
            return Err(file_error(
                path,
                format!(
                    "line {} needs name, original.fos, viewer.txt, quests, actors (tab-separated)",
                    i + 1
                ),
            ));
        }
        let name = columns[0].trim().to_string();
        if !seen.insert(name.clone()) {
            return Err(file_error(path, format!("duplicate checkpoint '{name}'")));
        }
        rows.push(Checkpoint {
            name,
            original: base.join(columns[1].trim()),
            viewer: base.join(columns[2].trim()),
            quests: ids(order, columns[3])?,
            actors: ids(order, columns[4])?,
        });
    }
    if rows.is_empty() {
        return Err(file_error(path, "no checkpoints"));
    }
    Ok(rows)
}

/// Convert a form in a native save into the current Data folder's load order.
fn loaded_id(order: &LoadOrder, save: &Save<'_>, reference: RefId) -> Option<FormId> {
    let raw = save.form_id(reference)?;
    if (1..=0x7ff).contains(&raw) || raw >> 24 == 0xff {
        return Some(FormId(raw));
    }
    let plugin = save.plugins.get((raw >> 24) as usize)?;
    let loaded = order
        .plugins()
        .iter()
        .find(|candidate| candidate.name.eq_ignore_ascii_case(plugin))?;
    Some(FormId(
        u32::from(loaded.load_index) << 24 | (raw & 0x00ff_ffff),
    ))
}

fn native_id(order: &LoadOrder, save: &Save<'_>, id: FormId) -> Option<u32> {
    if (1..=0x7ff).contains(&id.0) || id.0 >> 24 == 0xff {
        return Some(id.0);
    }
    let plugin = order.slot_name(id.mod_index())?;
    let index = save
        .plugins
        .iter()
        .position(|name| name.eq_ignore_ascii_case(plugin))?;
    Some((u32::try_from(index).ok()? << 24) | id.object_id())
}

/// The running package saved in the native actor process (`ActorPackage`,
/// FalloutNV.exe 1.4.0.525, 008c65d0; `fos::decode::actor_form`). `None`
/// means no process was saved or it could not be decoded; `Some(None)` means
/// the saved process explicitly had no package.
fn native_package(order: &LoadOrder, save: &Save<'_>, actor: FormId) -> Option<Option<FormId>> {
    let cf = save.change_form_of(native_id(order, save, actor)?)?;
    let form = decode::actor_form(cf, actor == PLAYER_REF).ok()?;
    match form.actor.mobile.process?.package {
        Some(reference) => loaded_id(order, save, reference).map(Some),
        None => Some(None),
    }
}

fn field(
    out: &mut impl Write,
    label: &str,
    original: &str,
    viewer: &str,
) -> Result<usize, CliError> {
    if original == viewer {
        writeln!(out, "  = {label}: {original}")?;
        Ok(0)
    } else {
        writeln!(out, "  ! {label}: original {original}; nv-rs {viewer}")?;
        Ok(1)
    }
}

fn measured<T: std::fmt::Debug + PartialEq>(
    out: &mut impl Write,
    label: &str,
    original: Option<T>,
    viewer: Option<T>,
) -> Result<usize, CliError> {
    if original.is_none() || viewer.is_none() {
        writeln!(
            out,
            "  ! {label} unavailable: original {original:?}; nv-rs {viewer:?}"
        )?;
        return Ok(1);
    }
    if original == viewer {
        writeln!(out, "  = {label}: {original:?}")?;
        Ok(0)
    } else {
        writeln!(out, "  ! {label}: original {original:?}; nv-rs {viewer:?}")?;
        Ok(1)
    }
}

fn position(
    out: &mut impl Write,
    label: &str,
    order: &LoadOrder,
    original: &GameState,
    viewer: &GameState,
    actor: FormId,
) -> Result<usize, CliError> {
    let left = original.place(order, actor);
    let right = viewer.place(order, actor);
    if left.is_none() || right.is_none() {
        writeln!(
            out,
            "  ! {label} position unavailable: original {left:?}; nv-rs {right:?}"
        )?;
        return Ok(1);
    }
    let mut differences = field(
        out,
        &format!("{label} place"),
        &format!("{:?}", left.map(|p| (p.0, p.1))),
        &format!("{:?}", right.map(|p| (p.0, p.1))),
    )?;
    for axis in 0..3 {
        differences += field(
            out,
            &format!("{label} {}", ["x", "y", "z"][axis]),
            &format!("{:?}", left.map(|p| p.2[axis])),
            &format!("{:?}", right.map(|p| p.2[axis])),
        )?;
    }
    Ok(differences)
}

fn compare(out: &mut impl Write, order: &LoadOrder, point: &Checkpoint) -> Result<usize, CliError> {
    let original_bytes = read(&point.original)?;
    let save = Save::parse(&original_bytes).map_err(|e| file_error(&point.original, e))?;
    let imported =
        world::fos_import::import_save(order, &save).map_err(|e| file_error(&point.original, e))?;
    if !imported.report.missing_plugins.is_empty() || !imported.report.failures.is_empty() {
        return Err(file_error(
            &point.original,
            format!(
                "incomplete import: missing plugins {:?}, decode failures {:?}",
                imported.report.missing_plugins, imported.report.failures
            ),
        ));
    }
    let viewer_bytes = read(&point.viewer)?;
    let viewer_text =
        std::str::from_utf8(&viewer_bytes).map_err(|e| file_error(&point.viewer, e))?;
    let (mut viewer, viewer_place) =
        world::save::load(viewer_text).map_err(|e| file_error(&point.viewer, e))?;
    if let Some(place) = viewer_place {
        viewer.player_cell = Some(place.cell);
        viewer.player_world = place.world;
        viewer.player_position = Some(place.position);
        viewer.player_heading = place.heading;
    }
    let original = &imported.state;
    writeln!(out, "{}:", point.name)?;
    let mut differences = position(out, "player", order, original, &viewer, PLAYER_REF)?;
    differences += measured(
        out,
        "game hour",
        original.global(order, "GameHour"),
        viewer.global(order, "GameHour"),
    )?;
    differences += measured(
        out,
        "player health",
        world::combat::health(order, original, PLAYER_REF),
        world::combat::health(order, &viewer, PLAYER_REF),
    )?;
    for &quest in &point.quests {
        differences += field(
            out,
            &format!("quest {quest} stage"),
            &original
                .stages
                .get(&quest)
                .copied()
                .unwrap_or(0)
                .to_string(),
            &viewer.stages.get(&quest).copied().unwrap_or(0).to_string(),
        )?;
    }
    for &actor in &point.actors {
        differences += position(
            out,
            &format!("actor {actor}"),
            order,
            original,
            &viewer,
            actor,
        )?;
        differences += measured(
            out,
            &format!("actor {actor} health"),
            world::combat::health(order, original, actor),
            world::combat::health(order, &viewer, actor),
        )?;
        match native_package(order, &save, actor) {
            Some(package) => {
                let current = world::ai::current_package(order, &viewer, actor).map(|p| p.form_id);
                differences += field(
                    out,
                    &format!("actor {actor} package (nv-rs derived)"),
                    &format!("{package:?}"),
                    &format!("{current:?}"),
                )?;
            }
            None => {
                writeln!(
                    out,
                    "  ! actor {actor} package: no decodable native process"
                )?;
                differences += 1;
            }
        }
    }
    writeln!(out, "  {differences} differences")?;
    Ok(differences)
}

pub fn run(out: &mut impl Write, rest: &[String]) -> Result<(), CliError> {
    if rest.len() != 2 {
        return Err(CliError::Usage(
            "compare-route needs <DATA> <ROUTE.tsv>".into(),
        ));
    }
    let data = Path::new(&rest[0]);
    let manifest = Path::new(&rest[1]);
    let order = LoadOrder::from_data_dir(data, &ActivePlugins::OfficialOnly)?;
    let points = checkpoints(&order, manifest)?;
    let mut differences = 0;
    for point in &points {
        differences += compare(out, &order, point)?;
    }
    writeln!(
        out,
        "{} checkpoints, {differences} differences",
        points.len()
    )?;
    out.flush()?;
    if differences != 0 {
        return Err(CliError::NotFound(format!(
            "route differs at {differences} fields"
        )));
    }
    Ok(())
}
