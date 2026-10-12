# Route comparison checkpoints (M10 / #22)

The first M10 tool compares saved checkpoints from the same route. It uses
the original game's own `.fos` saves and nv-rs's F12 `state.txt` snapshots.
The tool reads both files; it does not change the game installation or copy
game data into the repository. Keep captures and the manifest outside Git.

## Capture a route

1. Use the same installed Data files and load order for both runs. At each
   chosen point in the original Windows game, make a normal save and copy its
   `.fos` file to a private capture folder.
2. At the same route point in nv-rs, press F12. A scripted viewer run can
   schedule it with `--key-at 50 F12` (50 game seconds after placement).
   The existing Windows acceptance driver can request several captures with
   `-CaptureAt 10,30,45` (for example, with `-Routes doc`). F12 writes its
   `reports/NNN` folders under the working directory from which the driver
   was launched.
   Copy `reports/NNN/state.txt` as `tester.txt` and its `actors.tsv` as
   `tester.actors.tsv` for that checkpoint (and use matching stems for the
   others). F12 also writes a picture and human report, useful for checking
   that the two checkpoints really align. The actor sidecar holds the live
   loaded actors' positions and selected packages; the command requires it
   when the manifest names actors.
3. Put a tab-separated `route.tsv` in the folder. Each line has five fields:
   checkpoint name, original `.fos` path, nv-rs `state.txt` path, comma-separated
   quest IDs/editor IDs, and comma-separated actor reference IDs/editor IDs.
   Paths are relative to the manifest. Use `player` only for an actor field;
   player position and health are compared at every checkpoint already.

For example, a Doc route with two checkpoints can use these lines (the
separators between fields must be tabs):

```text
tester	tester.fos	tester.txt	VCG01	DocMitchellREF
door	door.fos	door.txt	VCG01	DocMitchellREF
```

Run `nvinspect compare-route <DATA FOLDER> <ROUTE.tsv>`. It reports each
field from the original save and the nv-rs snapshot, then a difference count.
It prints SHA-256 hashes of both input files and the native save's format,
number and play time so a result can be tied to the exact captures.
The command exits with an error if any compared field differs or a requested
position/package is unavailable. Quests not named in the manifest are not
compared. Use the game's `FalloutNV.esm` and official DLC files; an original
save with plugins outside that set is rejected rather than silently compared
under a different form ID map.

## What the numbers mean

- Positions are the game's own coordinate units. The comparison also checks
  the cell/worldspace IDs. For each requested quest, it checks the current
  stage, all stages marked done, and running/completed/failed flags. The
  manifest rejects names or IDs that do not identify quests or placed actors.
  Package form IDs must match exactly. There is no positional or time
  tolerance yet; the output gives raw values so any later tolerance can be
  based on captures.
- The original package is the `ActorPackage` in a saved actor process,
  decoded from the `.fos` file (`FalloutNV.exe` 1.4.0.525, `008c65d0`).
  The nv-rs package comes from the live viewer `Walker` at F12, and so does
  its actor position. A native save without a decodable process or an actor
  absent from the viewer capture is reported as unavailable.
- Health is calculated by the existing `world::fos_import` and `world::combat`
  paths. The importer does not yet carry active effects or all actor values
  from `.fos` (`world::fos_import::GAPS`), so a health match is provisional
  when those affect the actor. Check the original game's console or a live
  probe before calling such a case verified.
- A checkpoint pair measures state at two chosen moments. It does not prove
  that the same input sequence or frame timing produced those moments.

The next M10 batch should add live capture of the original actor state and
route input/timing metadata. Then calibrate
position and time tolerances from repeated original-game recordings.
