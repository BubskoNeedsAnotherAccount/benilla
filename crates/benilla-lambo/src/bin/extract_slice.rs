//! Asset Slice Extractor CLI for benilla-lambo Web / WASM deployment.
//!
//! Scans `time_trials.json` and Free Driving spawn points, samples all route corridors,
//! discovers all required ADT terrain tiles, WMO objects, M2 doodads, and BLP textures,
//! and extracts only this minimal slice from the WoW 1.12.1 MPQ patch chain into an
//! optimized folder or web bundle.

use std::collections::HashSet;
use std::fs;
use std::io::Cursor;
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use clap::Parser;
use serde::Deserialize;

use benilla_adt::{parse_adt, ParsedAdt};
use benilla_formats::Chain;
use benilla_m2::parse_m2;
use benilla_wdt::world_to_tile;
use benilla_wmo::{parse_wmo, ParsedWmo};

/// Extraction CLI options.
#[derive(Parser, Debug)]
#[command(
    name = "extract-slice",
    about = "Extract only the ADTs, models, and textures needed for benilla-lambo time trials into a web asset bundle",
    version
)]
struct Args {
    /// Path to WoW 1.12.1 Data directory holding MPQs. Defaults to $WOW_DATA.
    #[arg(short, long)]
    wow_data: Option<PathBuf>,

    /// Path to the time_trials.json definition file.
    #[arg(short, long, default_value = "crates/benilla-lambo/assets/time_trials.json")]
    trials: PathBuf,

    /// Destination directory for the extracted asset slice.
    #[arg(short, long, default_value = "assets/web_slice")]
    output: PathBuf,

    /// Tile padding radius around each waypoint corridor (default: 1 tile margin).
    #[arg(short, long, default_value_t = 1)]
    padding: i32,
}

#[derive(Deserialize, Debug)]
#[allow(dead_code)]
struct RawTimeTrialDef {
    name: String,
    map_id: u32,
    start: [f32; 3],
    end: [f32; 3],
    #[serde(default)]
    rotation: Option<f32>,
}

const BASE_DBCS: &[&str] = &[
    "DBFilesClient\\Map.dbc",
    "DBFilesClient\\AreaTable.dbc",
    "DBFilesClient\\Light.dbc",
    "DBFilesClient\\LightParams.dbc",
    "DBFilesClient\\LightIntBand.dbc",
    "DBFilesClient\\LightFloatBand.dbc",
    "DBFilesClient\\LightSkybox.dbc",
    "DBFilesClient\\GroundEffectTexture.dbc",
    "DBFilesClient\\GroundEffectDoodad.dbc",
    "DBFilesClient\\ZoneIntroMusicTable.dbc",
    "DBFilesClient\\SoundWaterType.dbc",
    "DBFilesClient\\TerrainType.dbc",
];

const BASE_WDTS: &[&str] = &[
    "World\\Maps\\Azeroth\\Azeroth.wdt",
    "World\\Maps\\Kalimdor\\Kalimdor.wdt",
    "World\\Maps\\Azeroth\\Azeroth.wdl",
    "World\\Maps\\Kalimdor\\Kalimdor.wdl",
];

// Free Driving locations defined in benilla-lambo
const FREE_DRIVE_SPAWNS: &[(&str, u32, [f32; 3])] = &[
    ("Lakeshire (Redridge Mountains)", 0, [-9429.1, -2231.4, 110.0]),
    ("Crossroads (The Barrens)", 1, [-437.0, -2596.0, 96.0]),
    ("Goldshire (Elwynn Forest)", 0, [-9464.0, 62.0, 56.0]),
];

fn normalize_mpq_path(p: &str) -> String {
    p.replace('/', "\\")
}

fn path_for_output(base: &Path, mpq_path: &str) -> PathBuf {
    let clean = mpq_path.replace('\\', "/").trim_start_matches('/').to_string();
    base.join(clean)
}

fn sample_segment_tiles(
    start: [f32; 3],
    end: [f32; 3],
    padding: i32,
    out: &mut HashSet<(u32, u32)>,
) {
    let dx = end[0] - start[0];
    let dy = end[1] - start[1];
    let dist = (dx * dx + dy * dy).sqrt();

    // Sample every ~50 yards to hit every tile along the route
    let steps = ((dist / 50.0).ceil() as usize).max(1);
    for i in 0..=steps {
        let t = i as f32 / steps as f32;
        let x = start[0] + dx * t;
        let y = start[1] + dy * t;
        let (tx, ty) = world_to_tile(x, y);

        for px in -padding..=padding {
            for py in -padding..=padding {
                let nx = tx as i32 + px;
                let ny = ty as i32 + py;
                if (0..64).contains(&nx) && (0..64).contains(&ny) {
                    out.insert((nx as u32, ny as u32));
                }
            }
        }
    }
}

fn extract_file(chain: &Chain, mpq_path: &str, output_dir: &Path) -> Result<Option<usize>> {
    let internal = normalize_mpq_path(mpq_path);
    if !chain.contains(&internal) {
        return Ok(None);
    }

    let bytes = chain.read(&internal)?;
    let out_path = path_for_output(output_dir, &internal);
    if let Some(parent) = out_path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(&out_path, &bytes)?;
    Ok(Some(bytes.len()))
}

fn main() -> Result<()> {
    let args = Args::parse();

    println!("============================================================");
    println!("🏎️  BENILLA-LAMBO WEB ASSET SLICE EXTRACTOR");
    println!("============================================================");

    // Resolve WoW data directory
    let wow_data_path = if let Some(p) = args.wow_data {
        p
    } else if let Ok(env_val) = std::env::var("WOW_DATA") {
        PathBuf::from(env_val)
    } else {
        match benilla_formats::wow_data() {
            Some(p) => p,
            None => {
                bail!(
                    "No WoW Data directory found! Specify via --wow-data <PATH> or set $env:WOW_DATA"
                );
            }
        }
    };

    println!("• WoW Data Path:   {}", wow_data_path.display());
    println!("• Trials JSON:     {}", args.trials.display());
    println!("• Output Folder:   {}", args.output.display());
    println!("• Tile Padding:    {} tile(s)", args.padding);

    // Open MPQ patch chain
    println!("\n[1/5] Opening WoW 1.12.1 MPQ patch chain...");
    let chain = Chain::open(&wow_data_path)
        .with_context(|| format!("Opening MPQ chain at {}", wow_data_path.display()))?;
    println!("✓ Chain mounted successfully (Locale: {:?})", chain.locale());

    // Parse time_trials.json
    let trials_json = fs::read_to_string(&args.trials)
        .with_context(|| format!("Reading time trials from {}", args.trials.display()))?;
    let trials: Vec<RawTimeTrialDef> = serde_json::from_str(&trials_json)
        .with_context(|| format!("Parsing {}", args.trials.display()))?;

    // Gather required ADT tiles per map
    println!("\n[2/5] Calculating required ADT terrain tiles...");
    let mut map_tiles: HashSet<(u32, u32, u32)> = HashSet::new(); // (map_id, tx, ty)

    for trial in &trials {
        let mut tiles = HashSet::new();
        sample_segment_tiles(trial.start, trial.end, args.padding, &mut tiles);
        println!(
            "  • Trial '{}' (Map {}): {} tiles",
            trial.name,
            trial.map_id,
            tiles.len()
        );
        for (tx, ty) in tiles {
            map_tiles.insert((trial.map_id, tx, ty));
        }
    }

    // Add free-drive spawns
    for (name, map_id, coords) in FREE_DRIVE_SPAWNS {
        let (tx, ty) = world_to_tile(coords[0], coords[1]);
        let mut spawn_tiles = HashSet::new();
        for px in -args.padding..=args.padding {
            for py in -args.padding..=args.padding {
                let nx = tx as i32 + px;
                let ny = ty as i32 + py;
                if (0..64).contains(&nx) && (0..64).contains(&ny) {
                    spawn_tiles.insert((nx as u32, ny as u32));
                }
            }
        }
        println!(
            "  • Spawn '{}' (Map {}): {} tiles",
            name,
            map_id,
            spawn_tiles.len()
        );
        for (tx, ty) in spawn_tiles {
            map_tiles.insert((*map_id, tx, ty));
        }
    }

    println!("✓ Total unique ADT tiles to extract: {}", map_tiles.len());

    let mut total_extracted_bytes: usize = 0;
    let mut extracted_files_count: usize = 0;
    let mut discovered_wmos: HashSet<String> = HashSet::new();
    let mut discovered_models: HashSet<String> = HashSet::new();
    let mut discovered_textures: HashSet<String> = HashSet::new();

    // 3. Extract Core DBCs and WDTs
    println!("\n[3/5] Extracting base DBC catalogs and WDT headers...");
    for dbc in BASE_DBCS {
        if let Some(sz) = extract_file(&chain, dbc, &args.output)? {
            total_extracted_bytes += sz;
            extracted_files_count += 1;
        }
    }
    for wdt in BASE_WDTS {
        if let Some(sz) = extract_file(&chain, wdt, &args.output)? {
            total_extracted_bytes += sz;
            extracted_files_count += 1;
        }
    }

    // 4. Extract ADT Tiles and inspect dependencies
    println!("\n[4/5] Extracting ADT tiles & discovering 3D object dependencies...");
    let mut adt_count = 0;
    for &(map_id, tx, ty) in &map_tiles {
        let map_name = match map_id {
            0 => "Azeroth",
            1 => "Kalimdor",
            _ => continue,
        };
        let adt_path = format!("World\\Maps\\{map_name}\\{map_name}_{tx}_{ty}.adt");
        if !chain.contains(&adt_path) {
            continue;
        }

        let bytes = chain.read(&adt_path)?;
        let out_path = path_for_output(&args.output, &adt_path);
        if let Some(parent) = out_path.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::write(&out_path, &bytes)?;
        total_extracted_bytes += bytes.len();
        extracted_files_count += 1;
        adt_count += 1;

        // Parse ADT to find textures, WMOs, and M2s
        if let Ok(ParsedAdt::Root(root)) = parse_adt(&mut Cursor::new(&bytes)) {
            for tex in root.textures {
                if !tex.is_empty() {
                    discovered_textures.insert(tex);
                }
            }
            for wmo in root.wmos {
                if !wmo.is_empty() {
                    discovered_wmos.insert(wmo);
                }
            }
            for model in root.models {
                if !model.is_empty() {
                    discovered_models.insert(model);
                }
            }
        }
    }
    println!(
        "✓ Extracted {} ADTs. Found {} WMOs, {} M2s, {} initial terrain textures.",
        adt_count,
        discovered_wmos.len(),
        discovered_models.len(),
        discovered_textures.len()
    );

    // 5. Extract WMOs (Roots + Groups) and M2s and all referenced BLPs
    println!("\n[5/5] Extracting WMO structures, M2 doodads, and BLP textures...");
    let mut wmo_extracted = 0;
    for wmo_path in &discovered_wmos {
        if let Some(sz) = extract_file(&chain, wmo_path, &args.output)? {
            total_extracted_bytes += sz;
            extracted_files_count += 1;
            wmo_extracted += 1;

            if let Ok(bytes) = chain.read(wmo_path) {
                if let Ok(ParsedWmo::Root(root)) = parse_wmo(&mut Cursor::new(&bytes)) {
                    for tex in root.textures {
                        if !tex.is_empty() {
                            discovered_textures.insert(tex);
                        }
                    }

                    // Extract all group files
                    let stem = wmo_path.strip_suffix(".wmo").unwrap_or(wmo_path);
                    for g in 0..root.n_groups {
                        let group_path = format!("{stem}_{g:03}.wmo");
                        if let Some(gsz) = extract_file(&chain, &group_path, &args.output)? {
                            total_extracted_bytes += gsz;
                            extracted_files_count += 1;
                        }
                    }
                }
            }
        }
    }

    let mut m2_extracted = 0;
    for m2_path in &discovered_models {
        if let Some(sz) = extract_file(&chain, m2_path, &args.output)? {
            total_extracted_bytes += sz;
            extracted_files_count += 1;
            m2_extracted += 1;

            if let Ok(bytes) = chain.read(m2_path) {
                if let Ok(m2_fmt) = parse_m2(&mut Cursor::new(&bytes)) {
                    for tex in &m2_fmt.model().textures {
                        if let Ok(tex_name) = tex.filename.string.to_str() {
                            if !tex_name.is_empty() {
                                discovered_textures.insert(tex_name.to_string());
                            }
                        }
                    }
                }
            }
        }
    }

    let mut blp_extracted = 0;
    for tex_path in &discovered_textures {
        if let Some(sz) = extract_file(&chain, tex_path, &args.output)? {
            total_extracted_bytes += sz;
            extracted_files_count += 1;
            blp_extracted += 1;
        }
    }

    let total_mb = total_extracted_bytes as f64 / (1024.0 * 1024.0);
    println!("\n============================================================");
    println!("✨ EXTRACTION COMPLETE!");
    println!("============================================================");
    println!("• Total Files Extracted:  {}", extracted_files_count);
    println!("  - ADT Terrain Tiles:    {}", adt_count);
    println!("  - WMO World Objects:    {}", wmo_extracted);
    println!("  - M2 Doodad Models:     {}", m2_extracted);
    println!("  - BLP Textures:         {}", blp_extracted);
    println!("• Total Uncompressed:     {:.2} MB", total_mb);
    println!(
        "• Estimated Web Gzip:     ~{:.2} MB",
        total_mb * 0.38
    );
    println!("• Output Location:        {}", args.output.display());
    println!("============================================================");

    Ok(())
}
