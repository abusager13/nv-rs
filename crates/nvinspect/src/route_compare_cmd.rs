//! Compare route checkpoints captured as original `.fos` saves and nv-rs's
//! own F12 `state.txt` and `actors.tsv` files. The original saves are decoded
//! by `fos` and `world::fos_import`; neither input is modified.

use std::collections::{BTreeMap, BTreeSet};
use std::io::Write;
use std::path::{Path, PathBuf};

use esm::{sig, ActivePlugins, FormId, FourCC, LoadOrder};
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

#[derive(Clone, Copy)]
struct ObservedActor {
    package: Option<FormId>,
    position: [f32; 3],
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
        .form_by_editor_id(token)
        .ok_or_else(|| CliError::NotFound(format!("form '{token}' is not in the Data folder")))
}

fn ids(order: &LoadOrder, value: &str, kinds: &[FourCC]) -> Result<Vec<FormId>, CliError> {
    value
        .split(',')
        .filter(|token| !token.trim().is_empty())
        .map(|token| {
            let id = resolve(order, token.trim())?;
            if id == PLAYER_REF && kinds.contains(&sig::ACHR) {
                return Ok(id);
            }
            match order.get(id) {
                Some(record) if kinds.contains(&record.entry.header.kind) => Ok(id),
                _ => Err(CliError::Usage(format!(
                    "'{token}' is not a {} record in the Data folder",
                    if kinds.len() == 1 && kinds[0] == sig::QUST {
                        "quest"
                    } else {
                        "placed actor"
                    }
                ))),
            }
        })
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
            quests: ids(order, columns[3], &[sig::QUST])?,
            actors: ids(order, columns[4], &[sig::ACHR, sig::ACRE])?,
        });
    }
    if rows.is_empty() {
        return Err(file_error(path, "no checkpoints"));
    }
    Ok(rows)
}

/// The F12 sidecar records the package selected by a live viewer Walker.
/// Its path is `<snapshot stem>.actors.tsv`, beside the copied snapshot.
fn observed_actors(snapshot: &Path) -> Result<BTreeMap<FormId, ObservedActor>, CliError> {
    let path = snapshot.with_extension("actors.tsv");
    let bytes = read(&path)?;
    let text = std::str::from_utf8(&bytes).map_err(|e| file_error(&path, e))?;
    let mut lines = text.lines();
    if lines.next() != Some("nv-rs actors 1") {
        return Err(file_error(&path, "not an nv-rs F12 actors snapshot"));
    }
    let mut actors = BTreeMap::new();
    for (index, line) in lines.enumerate() {
        let fields: Vec<_> = line.split('\t').collect();
        if fields.len() != 5 {
            return Err(file_error(
                &path,
                format!("line {} has no actor record", index + 2),
            ));
        }
        let form = |word: &str| u32::from_str_radix(word, 16).map(FormId);
        let actor = form(fields[0])
            .map_err(|_| file_error(&path, format!("line {} has a bad actor ID", index + 2)))?;
        let package = if fields[1] == "-" {
            None
        } else {
            Some(form(fields[1]).map_err(|_| {
                file_error(&path, format!("line {} has a bad package ID", index + 2))
            })?)
        };
        let mut position = [0.0; 3];
        for (axis, word) in position.iter_mut().zip(&fields[2..]) {
            *axis = word
                .parse::<f32>()
                .ok()
                .filter(|n| n.is_finite())
                .ok_or_else(|| {
                    file_error(&path, format!("line {} has a bad position", index + 2))
                })?;
        }
        if actors
            .insert(actor, ObservedActor { package, position })
            .is_some()
        {
            return Err(file_error(
                &path,
                format!("line {} repeats actor {actor}", index + 2),
            ));
        }
    }
    Ok(actors)
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
    observed: Option<[f32; 3]>,
) -> Result<usize, CliError> {
    let left = original.place(order, actor);
    let right = viewer
        .place(order, actor)
        .map(|(space, cell, place, heading)| (space, cell, observed.unwrap_or(place), heading));
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
    let observed = if point.actors.is_empty() {
        BTreeMap::new()
    } else {
        observed_actors(&point.viewer)?
    };
    writeln!(out, "{}:", point.name)?;
    writeln!(
        out,
        "  original sha256 {}",
        crate::sha256::hex_digest(&original_bytes)
    )?;
    writeln!(
        out,
        "  nv-rs sha256 {}",
        crate::sha256::hex_digest(&viewer_bytes)
    )?;
    writeln!(
        out,
        "  native save format {:#x}.{:x}, number {}, played {}",
        save.header.version, save.minor_version, save.header.save_number, save.header.play_time
    )?;
    let mut differences = position(out, "player", order, original, &viewer, PLAYER_REF, None)?;
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
        let stages = |state: &GameState| {
            let mut done: Vec<_> = state
                .stages_done
                .iter()
                .filter_map(|&(id, stage)| (id == quest).then_some(stage))
                .collect();
            done.sort_unstable();
            done
        };
        differences += field(
            out,
            &format!("quest {quest} stages done"),
            &format!("{:?}", stages(original)),
            &format!("{:?}", stages(&viewer)),
        )?;
        for (label, left, right) in [
            (
                "running",
                original.running.contains(&quest),
                viewer.running.contains(&quest),
            ),
            (
                "completed",
                original.completed.contains(&quest),
                viewer.completed.contains(&quest),
            ),
            (
                "failed",
                original.failed.contains(&quest),
                viewer.failed.contains(&quest),
            ),
        ] {
            differences += field(
                out,
                &format!("quest {quest} {label}"),
                &left.to_string(),
                &right.to_string(),
            )?;
        }
    }
    for &actor in &point.actors {
        let Some(live) = observed.get(&actor) else {
            writeln!(out, "  ! actor {actor}: absent from live nv-rs F12 capture")?;
            differences += 1;
            continue;
        };
        differences += position(
            out,
            &format!("actor {actor}"),
            order,
            original,
            &viewer,
            actor,
            Some(live.position),
        )?;
        differences += measured(
            out,
            &format!("actor {actor} health"),
            world::combat::health(order, original, actor),
            world::combat::health(order, &viewer, actor),
        )?;
        match native_package(order, &save, actor) {
            Some(package) => {
                differences += field(
                    out,
                    &format!("actor {actor} package"),
                    &format!("{package:?}"),
                    &format!("{:?}", live.package),
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
